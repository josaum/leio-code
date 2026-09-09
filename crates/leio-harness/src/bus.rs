//! Semantic coordination bus: agents exchange embedding vectors over Apache
//! Arrow Flight instead of human-readable text. Payloads are raw little-endian
//! f32 bytes moved through Arrow buffers — zero-copy on both write and read.
use anyhow::{Context, Result as AnyResult, bail};
use arrow_array::{
    ArrayRef, BinaryArray, Int64Array, RecordBatch, StringArray, UInt32Array, UInt64Array,
};
use arrow_flight::decode::{DecodedPayload, FlightDataDecoder};
use arrow_flight::encode::FlightDataEncoderBuilder;
use arrow_flight::error::FlightError;
use arrow_flight::flight_service_server::{FlightService, FlightServiceServer};
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightInfo,
    HandshakeRequest, HandshakeResponse, PollInfo, PutResult, SchemaResult, Ticket,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use futures::{TryStreamExt, stream};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tonic::codegen::tokio_stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

type BoxFlightStream<T> =
    Pin<Box<dyn tokio_stream::Stream<Item = std::result::Result<T, Status>> + Send + 'static>>;

pub fn embedding_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("seq", DataType::UInt64, false),
        Field::new("timestamp_ms", DataType::Int64, false),
        Field::new("agent_id", DataType::Utf8, false),
        Field::new("run_id", DataType::Utf8, false),
        Field::new("topic", DataType::Utf8, false),
        Field::new("dimension", DataType::UInt32, false),
        Field::new("vector", DataType::Binary, false), // raw little-endian f32 bytes
    ]))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRow {
    #[serde(default)] // server assigns
    pub seq: u64,
    #[serde(default)] // server assigns
    pub timestamp_ms: i64,
    pub agent_id: String,
    pub run_id: String,
    pub topic: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchHit {
    pub seq: u64,
    pub agent_id: String,
    pub run_id: String,
    pub topic: String,
    pub score: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MatchRequest {
    pub vector: Vec<f32>,
    pub topic: Option<String>,
    pub top_k: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GetTicket {
    topic: Option<String>,
    since_seq: Option<u64>,
}

#[derive(Default)]
pub struct SemanticBus {
    rows: Arc<Mutex<Vec<EmbeddingRow>>>,
    next_seq: Arc<Mutex<u64>>,
    persist_path: Arc<Mutex<Option<PathBuf>>>,
    embed: Arc<Mutex<Option<Arc<crate::embed::EmbedClient>>>>,
}

impl SemanticBus {
    pub fn load_or_default(persist_path: Option<PathBuf>) -> AnyResult<Self> {
        let Some(path) = persist_path else {
            return Ok(Self::default());
        };
        let rows = if path.exists() {
            load_bus_snapshot(&path).unwrap_or_else(|error| {
                eprintln!(
                    "warning: failed to load bus snapshot {}: {error}",
                    path.display()
                );
                Vec::new()
            })
        } else {
            Vec::new()
        };
        let next = rows.iter().map(|row| row.seq).max().unwrap_or(0);
        Ok(Self {
            rows: Arc::new(Mutex::new(rows)),
            next_seq: Arc::new(Mutex::new(next)),
            persist_path: Arc::new(Mutex::new(Some(path))),
            embed: Arc::new(Mutex::new(
                crate::embed::EmbedClient::from_env().ok().map(Arc::new),
            )),
        })
    }

    fn persist_snapshot(&self) {
        let path = self.persist_path.lock().expect("persist lock").clone();
        let Some(path) = path else { return };
        let rows = Self::snapshot_rows(&self.rows);
        if let Err(error) = save_bus_snapshot(&path, &rows) {
            eprintln!("warning: bus snapshot persist failed: {error}");
        }
    }

    fn append_rows(
        rows_store: &Arc<Mutex<Vec<EmbeddingRow>>>,
        next_seq: &Arc<Mutex<u64>>,
        mut rows: Vec<EmbeddingRow>,
    ) -> AnyResult<u64> {
        let mut next = next_seq.lock().expect("bus seq lock");
        let mut stored = rows_store.lock().expect("bus rows lock");
        for row in &mut rows {
            *next += 1;
            row.seq = *next;
            if row.timestamp_ms == 0 {
                row.timestamp_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
            }
        }
        let last = *next;
        stored.extend(rows);
        Ok(last)
    }

    fn append_and_persist(&self, rows: Vec<EmbeddingRow>) -> AnyResult<u64> {
        let last = Self::append_rows(&self.rows, &self.next_seq, rows)?;
        self.persist_snapshot();
        Ok(last)
    }

    fn snapshot_rows(rows_store: &Arc<Mutex<Vec<EmbeddingRow>>>) -> Vec<EmbeddingRow> {
        rows_store.lock().expect("bus rows lock").clone()
    }
}

#[tonic::async_trait]
impl FlightService for SemanticBus {
    type HandshakeStream = BoxFlightStream<HandshakeResponse>;
    type ListFlightsStream = BoxFlightStream<FlightInfo>;
    type DoGetStream = BoxFlightStream<FlightData>;
    type DoPutStream = BoxFlightStream<PutResult>;
    type DoExchangeStream = BoxFlightStream<FlightData>;
    type DoActionStream = BoxFlightStream<arrow_flight::Result>;
    type ListActionsStream = BoxFlightStream<ActionType>;

    async fn handshake(
        &self,
        _request: Request<Streaming<HandshakeRequest>>,
    ) -> std::result::Result<Response<Self::HandshakeStream>, Status> {
        Err(Status::unimplemented("handshake"))
    }

    async fn list_flights(
        &self,
        _request: Request<Criteria>,
    ) -> std::result::Result<Response<Self::ListFlightsStream>, Status> {
        Err(Status::unimplemented("list_flights"))
    }

    async fn get_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> std::result::Result<Response<FlightInfo>, Status> {
        Err(Status::unimplemented("get_flight_info"))
    }

    async fn poll_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> std::result::Result<Response<PollInfo>, Status> {
        Err(Status::unimplemented("poll_flight_info"))
    }

    async fn get_schema(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> std::result::Result<Response<SchemaResult>, Status> {
        Err(Status::unimplemented("get_schema"))
    }

    async fn do_get(
        &self,
        request: Request<Ticket>,
    ) -> std::result::Result<Response<Self::DoGetStream>, Status> {
        let ticket: GetTicket = serde_json::from_slice(&request.into_inner().ticket)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let since = ticket.since_seq.unwrap_or(0);
        let rows: Vec<EmbeddingRow> = Self::snapshot_rows(&self.rows)
            .into_iter()
            .filter(|row| row.seq > since)
            .filter(|row| {
                ticket
                    .topic
                    .as_ref()
                    .is_none_or(|topic| &row.topic == topic)
            })
            .collect();
        let batch = rows_to_batch(&rows).map_err(|error| Status::internal(error.to_string()))?;
        let encoded = FlightDataEncoderBuilder::new()
            .with_schema(embedding_schema())
            .build(stream::iter([Ok(batch)]))
            .map_err(|error: FlightError| Status::internal(error.to_string()));
        Ok(Response::new(Box::pin(encoded)))
    }

    async fn do_put(
        &self,
        request: Request<Streaming<FlightData>>,
    ) -> std::result::Result<Response<Self::DoPutStream>, Status> {
        let stream = request.into_inner().map_err(FlightError::from);
        let mut decoder = FlightDataDecoder::new(stream);
        let mut rows = Vec::new();
        while let Some(payload) = decoder
            .try_next()
            .await
            .map_err(|error| Status::internal(error.to_string()))?
        {
            if let DecodedPayload::RecordBatch(batch) = payload.payload {
                rows.extend(
                    batch_to_rows(&batch)
                        .map_err(|error| Status::invalid_argument(error.to_string()))?,
                );
            }
        }
        let last = self
            .append_and_persist(rows)
            .map_err(|error| Status::internal(error.to_string()))?;
        let result = PutResult {
            app_metadata: format!("{{\"last_seq\":{last}}}").into_bytes().into(),
        };
        Ok(Response::new(Box::pin(tokio_stream::once(Ok(result)))))
    }

    /// Bidirectional agent session. The bus owns the embedding schema, so no
    /// decoder guessing is needed:
    /// - FlightData with `app_metadata` `{"type":"match",...}` → semantic match
    ///   response in `app_metadata`.
    /// - FlightData schema messages (empty body) → ignored (schema is static).
    /// - FlightData with a non-empty body → decoded with the static schema via
    ///   `flight_data_to_arrow_batch`, appended, and acked in `app_metadata`.
    async fn do_exchange(
        &self,
        request: Request<Streaming<FlightData>>,
    ) -> std::result::Result<Response<Self::DoExchangeStream>, Status> {
        let rows = self.rows.clone();
        let next_seq = self.next_seq.clone();
        let persist_path = self.persist_path.clone();
        let embed = self.embed.clone();
        let mut inbound = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel::<std::result::Result<FlightData, Status>>(64);
        tokio::spawn(async move {
            let schema = embedding_schema();
            let dictionaries = std::collections::HashMap::new();
            loop {
                let message = match inbound.message().await {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                let metadata = message.app_metadata.clone();
                let message_type = exchange_message_type(&metadata);
                if message_type.as_deref() == Some("embed") {
                    let response = match handle_embed(&embed, &metadata).await {
                        Ok(body) => body,
                        Err(error) => {
                            format!("{{\"type\":\"error\",\"message\":{error:?}}}").into_bytes()
                        }
                    };
                    if tx.send(Ok(metadata_message(&response))).await.is_err() {
                        break;
                    }
                    continue;
                }
                if message_type.as_deref() == Some("evolve") {
                    let response = match handle_evolve(&rows, &next_seq, &persist_path, &metadata) {
                        Ok(body) => body,
                        Err(error) => {
                            format!("{{\"type\":\"error\",\"message\":{error:?}}}").into_bytes()
                        }
                    };
                    if tx.send(Ok(metadata_message(&response))).await.is_err() {
                        break;
                    }
                    continue;
                }
                if message_type.as_deref() == Some("merge_gate") {
                    let response = match handle_merge_gate(&rows, &metadata) {
                        Ok(body) => body,
                        Err(error) => {
                            format!("{{\"type\":\"error\",\"message\":{error:?}}}").into_bytes()
                        }
                    };
                    if tx.send(Ok(metadata_message(&response))).await.is_err() {
                        break;
                    }
                    continue;
                }
                if message_type.as_deref() == Some("match") {
                    let response = match handle_exchange_match(&rows, &metadata) {
                        Ok(body) => body,
                        Err(error) => {
                            format!("{{\"type\":\"error\",\"message\":{error:?}}}").into_bytes()
                        }
                    };
                    if tx.send(Ok(metadata_message(&response))).await.is_err() {
                        break;
                    }
                    continue;
                }
                if message.data_body.is_empty() {
                    continue; // schema/heartbeat frame
                }
                let decoded = arrow_flight::utils::flight_data_to_arrow_batch(
                    &message,
                    schema.clone(),
                    &dictionaries,
                )
                .map_err(|error| anyhow::anyhow!(error.to_string()))
                .and_then(|batch| batch_to_rows(&batch))
                .and_then(|parsed| {
                    let last = Self::append_rows(&rows, &next_seq, parsed)?;
                    if let Some(path) = persist_path.lock().expect("persist lock").clone()
                        && let Err(error) = save_bus_snapshot(&path, &Self::snapshot_rows(&rows))
                    {
                        eprintln!("warning: bus snapshot persist failed: {error}");
                    }
                    Ok(last)
                });
                let body = match decoded {
                    Ok(last) => format!("{{\"type\":\"ack\",\"last_seq\":{last}}}").into_bytes(),
                    Err(error) => {
                        format!("{{\"type\":\"error\",\"message\":{error:?}}}").into_bytes()
                    }
                };
                if tx.send(Ok(metadata_message(&body))).await.is_err() {
                    break;
                }
            }
        });
        let outbound = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(outbound)))
    }

    async fn do_action(
        &self,
        request: Request<Action>,
    ) -> std::result::Result<Response<Self::DoActionStream>, Status> {
        let action = request.into_inner();
        if action.r#type != "match" {
            return Err(Status::invalid_argument(format!(
                "unknown action: {}",
                action.r#type
            )));
        }
        let query: MatchRequest = serde_json::from_slice(&action.body)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if query.vector.is_empty() {
            return Err(Status::invalid_argument("vector must not be empty"));
        }
        let top_k = query.top_k.unwrap_or(5).clamp(1, 128);
        let snapshot = Self::snapshot_rows(&self.rows);
        let mut hits: Vec<(f32, &EmbeddingRow)> = snapshot
            .iter()
            .filter(|row| query.topic.as_ref().is_none_or(|topic| &row.topic == topic))
            .filter(|row| row.vector.len() == query.vector.len())
            .map(|row| (cosine_similarity(&query.vector, &row.vector), row))
            .collect();
        hits.sort_by(|left, right| right.0.total_cmp(&left.0));
        hits.truncate(top_k);
        let body = serde_json::to_vec(
            &hits
                .into_iter()
                .map(|(score, row)| MatchHit {
                    seq: row.seq,
                    agent_id: row.agent_id.clone(),
                    run_id: row.run_id.clone(),
                    topic: row.topic.clone(),
                    score,
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(Box::pin(tokio_stream::once(Ok(
            arrow_flight::Result { body: body.into() },
        )))))
    }

    async fn list_actions(
        &self,
        _request: Request<Empty>,
    ) -> std::result::Result<Response<Self::ListActionsStream>, Status> {
        Ok(Response::new(Box::pin(tokio_stream::iter([Ok(
            ActionType {
                r#type: "match".to_owned(),
                description: "cosine top-k over stored embedding vectors".to_owned(),
            },
        )]))))
    }
}

fn exchange_message_type(metadata: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(metadata)
        .ok()?
        .get("type")?
        .as_str()
        .map(str::to_owned)
}

pub async fn serve(bind: SocketAddr) -> AnyResult<()> {
    serve_persistent(bind, None).await
}

pub async fn serve_persistent(bind: SocketAddr, persist_path: Option<PathBuf>) -> AnyResult<()> {
    let bus = Arc::new(SemanticBus::load_or_default(persist_path)?);
    Server::builder()
        .add_service(FlightServiceServer::from_arc(bus))
        .serve(bind)
        .await
        .context("flight bus serve")
}

pub async fn serve_uds(path: PathBuf, persist_path: Option<PathBuf>) -> AnyResult<()> {
    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;

    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind UDS flight bus to {}", path.display()))?;
    let stream = UnixListenerStream::new(listener);

    let bus = Arc::new(SemanticBus::load_or_default(persist_path)?);
    Server::builder()
        .add_service(FlightServiceServer::from_arc(bus))
        .serve_with_incoming(stream)
        .await
        .context("flight bus serve over uds")
}

pub async fn serve_auto(bind: &str, persist_path: Option<PathBuf>) -> AnyResult<()> {
    if let Some(uds_path) = bind.strip_prefix("unix://") {
        serve_uds(PathBuf::from(uds_path), persist_path).await
    } else if bind.starts_with('/') || bind.ends_with(".sock") {
        serve_uds(PathBuf::from(bind), persist_path).await
    } else {
        let address: SocketAddr = bind
            .parse()
            .with_context(|| format!("invalid TCP bind address: {bind}"))?;
        serve_persistent(address, persist_path).await
    }
}

pub(crate) fn metadata_message(body: &[u8]) -> FlightData {
    FlightData {
        app_metadata: body.to_vec().into(),
        ..Default::default()
    }
}

#[derive(Debug, Deserialize)]
struct EvolveRequest {
    lane: String,
    parent_seq: Option<u64>,
    goal_vector: Vec<f32>,
    strength: Option<f32>,
    max_angle_deg: Option<f32>,
    anchor_beta: Option<f32>,
    seed: Option<u64>,
}

/// GEPA evolution on the bus: mutate an explicitly selected lane candidate, or
/// the lane's latest compatible intent for legacy callers, within a trust
/// region and publish the offspring.
fn handle_evolve(
    rows_store: &Arc<Mutex<Vec<EmbeddingRow>>>,
    next_seq: &Arc<Mutex<u64>>,
    persist_path: &Arc<Mutex<Option<PathBuf>>>,
    metadata: &[u8],
) -> AnyResult<Vec<u8>> {
    let request: EvolveRequest = serde_json::from_slice(metadata)?;
    if request.goal_vector.is_empty() {
        bail!("goal_vector must not be empty");
    }
    let snapshot = SemanticBus::snapshot_rows(rows_store);
    let intent_topic = format!("intent/{}", request.lane);
    let result_topic = format!("result/{}", request.lane);
    let parent = if let Some(parent_seq) = request.parent_seq {
        let parent = snapshot
            .iter()
            .find(|row| row.seq == parent_seq)
            .with_context(|| format!("parent_seq {parent_seq} does not exist"))?;
        if parent.topic != intent_topic && parent.topic != result_topic {
            bail!(
                "parent_seq {parent_seq} is not in intent/{} or result/{}",
                request.lane,
                request.lane
            );
        }
        if parent.vector.len() != request.goal_vector.len() {
            bail!(
                "parent_seq {parent_seq} dimension {} does not match goal dimension {}",
                parent.vector.len(),
                request.goal_vector.len()
            );
        }
        parent
    } else {
        snapshot
            .iter()
            .filter(|row| row.topic == intent_topic)
            .filter(|row| row.vector.len() == request.goal_vector.len())
            .max_by_key(|row| row.seq)
            .context("lane has no prior intent to evolve")?
    };
    let strength = request.strength.unwrap_or(0.1);
    let max_angle_deg = request.max_angle_deg.unwrap_or(5.0);
    let mut rng = crate::gepa::SplitMix64::new(request.seed.unwrap_or(0));
    let offspring =
        crate::gepa::trust_region_mutate(&parent.vector, strength, max_angle_deg, &mut rng);
    let anchor_penalty = request
        .anchor_beta
        .map(|beta| crate::gepa::anchor_penalty(&offspring, &request.goal_vector, beta));
    let run_id = format!("{}-evolved-{}", request.lane, std::process::id());
    let mut row = new_embedding_row(&parent.agent_id, &run_id, &intent_topic, offspring.clone());
    let last = SemanticBus::append_rows(rows_store, next_seq, vec![row.clone()])?;
    row.seq = last;
    if let Some(path) = persist_path.lock().expect("persist lock").clone()
        && let Err(error) = save_bus_snapshot(&path, &SemanticBus::snapshot_rows(rows_store))
    {
        eprintln!("warning: bus snapshot persist failed: {error}");
    }
    Ok(serde_json::to_vec(&serde_json::json!({
        "type": "evolve",
        "lane": request.lane,
        "parent_seq": parent.seq,
        "offspring_seq": last,
        "run_id": run_id,
        "offspring": offspring,
        "anchor_penalty": anchor_penalty,
    }))?)
}

#[derive(Debug, Deserialize)]
struct EmbedRequest {
    text: Vec<String>,
}

async fn handle_embed(
    embed: &Arc<Mutex<Option<Arc<crate::embed::EmbedClient>>>>,
    metadata: &[u8],
) -> AnyResult<Vec<u8>> {
    let request: EmbedRequest = serde_json::from_slice(metadata)?;
    if request.text.is_empty() {
        bail!("embed requires at least one text");
    }
    let client = embed
        .lock()
        .expect("embed lock")
        .clone()
        .context("embedding is not configured on this bus")?;
    let vectors = client.embed(&request.text).await?;
    Ok(serde_json::to_vec(&serde_json::json!({
        "type": "embed",
        "vectors": vectors,
    }))?)
}

#[derive(Debug, Deserialize)]
struct MergeGateRequest {
    lane: String,
    goal_vector: Vec<f32>,
    threshold: Option<f32>,
    beta: Option<f32>,
}

fn handle_merge_gate(
    rows_store: &Arc<Mutex<Vec<EmbeddingRow>>>,
    metadata: &[u8],
) -> AnyResult<Vec<u8>> {
    let request: MergeGateRequest = serde_json::from_slice(metadata)?;
    if request.goal_vector.is_empty() {
        bail!("goal_vector must not be empty");
    }
    let beta = request.beta.unwrap_or(1.0);
    let threshold = request.threshold.unwrap_or(0.0);
    let snapshot = SemanticBus::snapshot_rows(rows_store);
    let lane_topic = format!("intent/{}", request.lane);
    let result_topic = format!("result/{}", request.lane);
    let latest = snapshot
        .iter()
        .filter(|row| row.topic == lane_topic || row.topic == result_topic)
        .filter(|row| row.vector.len() == request.goal_vector.len())
        .max_by_key(|row| row.seq);
    let Some(latest) = latest else {
        return Ok(serde_json::to_vec(&serde_json::json!({
            "type": "merge_gate",
            "lane": request.lane,
            "verdict": "blocked",
            "reason": "no lane state on bus",
        }))?);
    };
    let penalty = crate::gepa::anchor_penalty(&latest.vector, &request.goal_vector, beta);
    let adjusted = 1.0 - penalty;
    let verdict = if adjusted >= threshold {
        "pass"
    } else {
        "blocked"
    };
    Ok(serde_json::to_vec(&serde_json::json!({
        "type": "merge_gate",
        "lane": request.lane,
        "verdict": verdict,
        "score": adjusted,
        "anchor_penalty": penalty,
        "threshold": threshold,
        "state_seq": latest.seq,
        "state_agent": latest.agent_id,
    }))?)
}

fn handle_exchange_match(
    rows_store: &Arc<Mutex<Vec<EmbeddingRow>>>,
    metadata: &[u8],
) -> AnyResult<Vec<u8>> {
    let query: MatchRequest = serde_json::from_slice(metadata)?;
    if query.vector.is_empty() {
        bail!("vector must not be empty");
    }
    let top_k = query.top_k.unwrap_or(5).clamp(1, 128);
    let snapshot = SemanticBus::snapshot_rows(rows_store);
    let mut hits: Vec<(f32, &EmbeddingRow)> = snapshot
        .iter()
        .filter(|row| query.topic.as_ref().is_none_or(|topic| &row.topic == topic))
        .filter(|row| row.vector.len() == query.vector.len())
        .map(|row| (cosine_similarity(&query.vector, &row.vector), row))
        .collect();
    hits.sort_by(|left, right| right.0.total_cmp(&left.0));
    hits.truncate(top_k);
    let hits: Vec<MatchHit> = hits
        .into_iter()
        .map(|(score, row)| MatchHit {
            seq: row.seq,
            agent_id: row.agent_id.clone(),
            run_id: row.run_id.clone(),
            topic: row.topic.clone(),
            score,
        })
        .collect();
    Ok(serde_json::to_vec(
        &serde_json::json!({ "type": "matches", "hits": hits }),
    )?)
}

pub(crate) fn save_bus_snapshot(path: &Path, rows: &[EmbeddingRow]) -> AnyResult<()> {
    use arrow_ipc::writer::FileWriter;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let batch = rows_to_batch(rows)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let file = std::fs::File::create(&temporary)?;
    let mut writer = FileWriter::try_new(file, &embedding_schema())?;
    writer.write(&batch)?;
    writer.finish()?;
    std::fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(crate) fn load_bus_snapshot(path: &Path) -> AnyResult<Vec<EmbeddingRow>> {
    use arrow_ipc::reader::FileReader;
    use memmap2::Mmap;
    use std::io::Cursor;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: the mapping is read-only and lives for the reader traversal.
    let mmap = unsafe { Mmap::map(&file)? };
    let reader = FileReader::try_new(Cursor::new(mmap.as_ref()), None)?;
    let mut rows = Vec::new();
    for batch in reader {
        rows.extend(batch_to_rows(&batch?)?);
    }
    rows.sort_by_key(|row| row.seq);
    Ok(rows)
}

pub(crate) fn rows_to_batch(rows: &[EmbeddingRow]) -> AnyResult<RecordBatch> {
    let schema = embedding_schema();
    let vectors: Vec<Vec<u8>> = rows
        .iter()
        .map(|row| {
            row.vector
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect()
        })
        .collect();
    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from(
            rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rows.iter().map(|row| row.timestamp_ms).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.agent_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.run_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|row| row.topic.as_str()),
        )),
        Arc::new(UInt32Array::from(
            rows.iter()
                .map(|row| row.vector.len() as u32)
                .collect::<Vec<_>>(),
        )),
        Arc::new(BinaryArray::from_iter_values(
            vectors.iter().map(Vec::as_slice),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

pub(crate) fn batch_to_rows(batch: &RecordBatch) -> AnyResult<Vec<EmbeddingRow>> {
    let agent_ids = string_column(batch, "agent_id")?;
    let run_ids = string_column(batch, "run_id")?;
    let topics = string_column(batch, "topic")?;
    let vectors = batch
        .column_by_name("vector")
        .context("missing vector column")?
        .as_any()
        .downcast_ref::<BinaryArray>()
        .context("vector column must be Binary")?;
    let timestamps = batch
        .column_by_name("timestamp_ms")
        .context("missing timestamp_ms column")?
        .as_any()
        .downcast_ref::<Int64Array>()
        .context("timestamp_ms column must be Int64")?;
    let seqs = batch
        .column_by_name("seq")
        .context("missing seq column")?
        .as_any()
        .downcast_ref::<UInt64Array>()
        .context("seq column must be UInt64")?;
    let mut rows = Vec::with_capacity(batch.num_rows());
    for index in 0..batch.num_rows() {
        let bytes = vectors.value(index);
        if bytes.len() % 4 != 0 {
            bail!("vector bytes length must be a multiple of 4");
        }
        let vector = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("chunk size")))
            .collect();
        rows.push(EmbeddingRow {
            seq: seqs.value(index),
            timestamp_ms: timestamps.value(index),
            agent_id: agent_ids.value(index).to_owned(),
            run_id: run_ids.value(index).to_owned(),
            topic: topics.value(index).to_owned(),
            vector,
        });
    }
    Ok(rows)
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> AnyResult<&'a StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("missing {name} column"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("{name} column must be Utf8"))
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

#[allow(dead_code)]
/// In-process round-trip check over a real do_exchange session.
pub async fn selftest(port: u16) -> AnyResult<serde_json::Value> {
    use arrow_flight::flight_service_client::FlightServiceClient;
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    tokio::spawn(serve(bind));
    let mut client = None;
    for _ in 0..50 {
        let endpoint = tonic::transport::Endpoint::new(format!("http://{bind}"))?;
        match endpoint.connect().await {
            Ok(channel) => {
                client = Some(FlightServiceClient::new(channel));
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    let mut client = client.context("bus did not come up")?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<FlightData>(16);
    let outbound = tokio_stream::wrappers::ReceiverStream::new(out_rx);
    let response = client.do_exchange(outbound).await?;
    let mut inbound = response.into_inner();
    let rows = vec![
        new_embedding_row("codex", "run-1", "repo/main", vec![1.0, 0.0, 0.0]),
        new_embedding_row("codex", "run-1", "intent/codex", vec![1.0, 0.0, 0.0]),
        new_embedding_row("kimi", "run-2", "repo/main", vec![0.9, 0.1, 0.0]),
        new_embedding_row("grok", "run-3", "repo/feature", vec![0.0, 0.0, 1.0]),
    ];
    let batch = rows_to_batch(&rows)?;
    let encoded: Vec<FlightData> = FlightDataEncoderBuilder::new()
        .with_schema(embedding_schema())
        .build(stream::iter([Ok(batch)]))
        .try_collect()
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    for message in encoded {
        out_tx.send(message).await?;
    }
    let ack = inbound
        .message()
        .await?
        .context("missing ack")?
        .app_metadata
        .to_vec();
    out_tx
        .send(metadata_message(
            br#"{"type":"match","vector":[1.0,0.0,0.0],"topic":"repo/main","top_k":2}"#,
        ))
        .await?;
    let matches = inbound
        .message()
        .await?
        .context("missing matches")?
        .app_metadata
        .to_vec();
    out_tx
        .send(metadata_message(
            br#"{"type":"merge_gate","lane":"codex","goal_vector":[1.0,0.0,0.0],"threshold":0.5}"#,
        ))
        .await?;
    let gate = inbound
        .message()
        .await?
        .context("missing merge gate verdict")?
        .app_metadata
        .to_vec();
    out_tx
        .send(metadata_message(
            br#"{"type":"evolve","lane":"codex","goal_vector":[1.0,0.0,0.0],"strength":0.2,"max_angle_deg":5,"seed":7,"anchor_beta":1.0}"#,
        ))
        .await?;
    let evolve = inbound
        .message()
        .await?
        .context("missing evolve response")?
        .app_metadata
        .to_vec();
    Ok(serde_json::json!({
        "ack": serde_json::from_slice::<serde_json::Value>(&ack)?,
        "matches": serde_json::from_slice::<serde_json::Value>(&matches)?,
        "merge_gate": serde_json::from_slice::<serde_json::Value>(&gate)?,
        "evolve": serde_json::from_slice::<serde_json::Value>(&evolve)?,
    }))
}

#[allow(dead_code)]
pub fn new_embedding_row(
    agent_id: &str,
    run_id: &str,
    topic: &str,
    vector: Vec<f32>,
) -> EmbeddingRow {
    EmbeddingRow {
        seq: 0,
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        agent_id: agent_id.to_owned(),
        run_id: run_id.to_owned(),
        topic: topic.to_owned(),
        vector,
    }
}

#[allow(dead_code)]
pub fn rows_to_flight_stream(
    rows: Vec<EmbeddingRow>,
) -> AnyResult<impl futures::Stream<Item = std::result::Result<FlightData, FlightError>>> {
    let batch = rows_to_batch(&rows)?;
    Ok(FlightDataEncoderBuilder::new()
        .with_schema(embedding_schema())
        .build(stream::iter([Ok(batch)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    type BusState = (
        Arc<Mutex<Vec<EmbeddingRow>>>,
        Arc<Mutex<u64>>,
        Arc<Mutex<Option<PathBuf>>>,
    );

    fn empty_bus() -> BusState {
        (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(None)),
        )
    }

    fn stored_row(seq: u64, agent_id: &str, topic: &str, vector: Vec<f32>) -> EmbeddingRow {
        let mut row = new_embedding_row(agent_id, "seed", topic, vector);
        row.seq = seq;
        row
    }

    fn bus_with_rows(rows: Vec<EmbeddingRow>) -> BusState {
        let next_seq = rows.iter().map(|row| row.seq).max().unwrap_or(0);
        (
            Arc::new(Mutex::new(rows)),
            Arc::new(Mutex::new(next_seq)),
            Arc::new(Mutex::new(None)),
        )
    }

    #[test]
    fn merge_gate_blocks_without_lane_state() {
        let (rows, _, _) = empty_bus();
        let body = br#"{"type":"merge_gate","lane":"x","goal_vector":[1.0,0.0],"threshold":0.5}"#;
        let response = handle_merge_gate(&rows, body).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(value["verdict"], "blocked");
        assert!(value["reason"].as_str().unwrap().contains("no lane state"));
    }

    #[test]
    fn evolve_requires_prior_intent() {
        let (rows, next, persist) = empty_bus();
        let body = br#"{"type":"evolve","lane":"x","goal_vector":[1.0,0.0]}"#;
        assert!(handle_evolve(&rows, &next, &persist, body).is_err());
    }

    #[test]
    fn evolve_accepts_explicit_result_parent() {
        let (rows, next, persist) = bus_with_rows(vec![
            stored_row(1, "intent-agent", "intent/x", vec![1.0, 0.0]),
            stored_row(2, "result-agent", "result/x", vec![0.0, 1.0]),
        ]);
        let body = br#"{"type":"evolve","lane":"x","parent_seq":2,"goal_vector":[0.0,1.0],"strength":0.0}"#;

        let response = handle_evolve(&rows, &next, &persist, body).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&response).unwrap();

        assert_eq!(value["parent_seq"], 2);
        assert_eq!(value["offspring_seq"], 3);
        let stored = rows.lock().unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored.last().unwrap().agent_id, "result-agent");
        assert_eq!(stored.last().unwrap().topic, "intent/x");
        assert_eq!(stored.last().unwrap().vector, vec![0.0, 1.0]);
    }

    #[test]
    fn evolve_without_parent_uses_latest_compatible_intent() {
        let (rows, next, persist) = bus_with_rows(vec![
            stored_row(1, "older-intent", "intent/x", vec![1.0, 0.0]),
            stored_row(2, "newer-result", "result/x", vec![0.0, 1.0]),
            stored_row(3, "latest-intent", "intent/x", vec![0.8, 0.2]),
            stored_row(4, "wrong-dimension", "intent/x", vec![1.0, 0.0, 0.0]),
        ]);
        let body = br#"{"type":"evolve","lane":"x","goal_vector":[1.0,0.0],"strength":0.0}"#;

        let response = handle_evolve(&rows, &next, &persist, body).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&response).unwrap();

        assert_eq!(value["parent_seq"], 3);
        assert_eq!(value["offspring_seq"], 5);
        assert_eq!(
            rows.lock().unwrap().last().unwrap().agent_id,
            "latest-intent"
        );
    }

    #[test]
    fn evolve_rejects_invalid_explicit_parent_without_append() {
        let (rows, next, persist) = bus_with_rows(vec![
            stored_row(1, "other-lane", "intent/y", vec![1.0, 0.0]),
            stored_row(2, "wrong-dimension", "result/x", vec![1.0, 0.0, 0.0]),
        ]);

        for body in [
            br#"{"type":"evolve","lane":"x","parent_seq":99,"goal_vector":[1.0,0.0]}"#.as_slice(),
            br#"{"type":"evolve","lane":"x","parent_seq":1,"goal_vector":[1.0,0.0]}"#.as_slice(),
            br#"{"type":"evolve","lane":"x","parent_seq":2,"goal_vector":[1.0,0.0]}"#.as_slice(),
        ] {
            assert!(handle_evolve(&rows, &next, &persist, body).is_err());
            assert_eq!(rows.lock().unwrap().len(), 2);
            assert_eq!(*next.lock().unwrap(), 2);
        }
    }

    #[test]
    fn match_rejects_empty_query_vector() {
        let (rows, _, _) = empty_bus();
        let body = br#"{"type":"match","vector":[]}"#;
        assert!(handle_exchange_match(&rows, body).is_err());
    }

    #[test]
    fn exchange_dispatch_is_independent_of_json_key_order() {
        let metadata = br#"{"top_k":null,"topic":null,"type":"match","vector":[1.0]}"#;
        assert_eq!(exchange_message_type(metadata).as_deref(), Some("match"));
    }

    #[cfg(feature = "codeview")]
    #[test]
    fn fingerprint_vector_is_normalized_and_stable() {
        let a = crate::codeview::fingerprint_vector("same");
        let b = crate::codeview::fingerprint_vector("same");
        assert_eq!(a, b);
        assert!((crate::gepa::dot(&a, &a) - 1.0).abs() < 1e-5);
    }
}
