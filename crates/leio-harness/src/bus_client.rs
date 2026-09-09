//! Client side of the semantic bus: one `do_exchange` session per call,
//! embedding frames out, `app_metadata` acknowledgements/matches in.
use crate::bus::{
    EmbeddingRow, MatchHit, MatchRequest, embedding_schema, metadata_message, rows_to_batch,
};
use anyhow::{Context, Result, bail};
use arrow_flight::encode::FlightDataEncoderBuilder;
use arrow_flight::error::FlightError;
use arrow_flight::flight_service_client::FlightServiceClient;
use futures::{TryStreamExt, stream};
use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvolveOptions {
    pub strength: f32,
    pub max_angle_deg: f32,
    pub anchor_beta: f32,
    pub seed: u64,
}

pub struct BusClient {
    client: FlightServiceClient<Channel>,
}

impl BusClient {
    pub async fn connect(addr: &str) -> Result<Self> {
        let channel = if let Some(uds_path) = addr.strip_prefix("unix://") {
            Self::connect_uds(PathBuf::from(uds_path)).await?
        } else if addr.starts_with('/') || addr.ends_with(".sock") {
            Self::connect_uds(PathBuf::from(addr)).await?
        } else {
            let endpoint = Endpoint::new(addr.to_owned())?.timeout(Duration::from_secs(30));
            endpoint.connect().await?
        };
        Ok(Self {
            client: FlightServiceClient::new(channel),
        })
    }

    async fn connect_uds(path: PathBuf) -> Result<Channel> {
        use hyper_util::rt::TokioIo;
        use tower::service_fn;
        let endpoint = Endpoint::try_from("http://[::]:50051")?.timeout(Duration::from_secs(30));
        let socket_path = path.clone();
        let channel = endpoint
            .connect_with_connector(service_fn(move |_: tonic::transport::Uri| {
                let p = socket_path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(p).await?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await
            .with_context(|| {
                format!("failed to connect to UDS flight bus at {}", path.display())
            })?;
        Ok(channel)
    }

    pub async fn connect_with_retry(addr: &str, attempts: usize, delay: Duration) -> Result<Self> {
        let mut last: Option<anyhow::Error> = None;
        for _ in 0..attempts.max(1) {
            match Self::connect(addr).await {
                Ok(client) => return Ok(client),
                Err(error) => {
                    last = Some(error);
                    tokio::time::sleep(delay).await;
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("bus connect failed")))
    }

    /// Publish embedding rows. Returns the assigned last sequence number.
    pub async fn publish(&mut self, rows: Vec<EmbeddingRow>) -> Result<u64> {
        if rows.is_empty() {
            bail!("publish requires at least one row");
        }
        let batch = rows_to_batch(&rows)?;
        let frames: Vec<arrow_flight::FlightData> = FlightDataEncoderBuilder::new()
            .with_schema(embedding_schema())
            .build(stream::iter([Ok(batch)]))
            .try_collect()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let session = self.client.do_exchange(tokio_stream::iter(frames)).await?;
        let mut inbound = session.into_inner();
        let ack = inbound
            .message()
            .await?
            .context("bus closed before ack")?
            .app_metadata;
        let ack: serde_json::Value = serde_json::from_slice(&ack)?;
        if ack["type"] == "error" {
            bail!("bus publish rejected: {}", ack["message"]);
        }
        ack["last_seq"].as_u64().context("ack missing last_seq")
    }

    /// Cosine top-k match over stored embeddings.
    pub async fn match_query(&mut self, query: MatchRequest) -> Result<Vec<MatchHit>> {
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "match",
            "vector": query.vector,
            "topic": query.topic,
            "top_k": query.top_k,
        }))?;
        let session = self
            .client
            .do_exchange(tokio_stream::iter([metadata_message(&body)]))
            .await?;
        let mut inbound = session.into_inner();
        let response = inbound
            .message()
            .await?
            .context("bus closed before match response")?
            .app_metadata;
        let parsed: serde_json::Value = serde_json::from_slice(&response)?;
        if parsed["type"] == "error" {
            bail!("bus match rejected: {}", parsed["message"]);
        }
        Ok(serde_json::from_value(parsed["hits"].clone())?)
    }
}

impl BusClient {
    /// List stored embedding rows for an optional topic via do_get.
    pub async fn list(&mut self, topic: Option<&str>) -> Result<Vec<EmbeddingRow>> {
        use arrow_flight::decode::{DecodedPayload, FlightDataDecoder};
        let ticket = serde_json::to_vec(&serde_json::json!({
            "topic": topic,
            "since_seq": 0,
        }))?;
        let response = self
            .client
            .do_get(arrow_flight::Ticket {
                ticket: ticket.into(),
            })
            .await?;
        let mut rows = Vec::new();
        let mut decoder = FlightDataDecoder::new(response.into_inner().map_err(FlightError::from));
        while let Some(payload) = decoder
            .try_next()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
        {
            if let DecodedPayload::RecordBatch(batch) = payload.payload {
                rows.extend(crate::bus::batch_to_rows(&batch)?);
            }
        }
        Ok(rows)
    }

    /// Embed text through the bus's own `do_exchange` embed op (inference runs
    /// server-side, on the lab GPU). Returns one vector per input text.
    pub async fn embed(&mut self, text: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if text.is_empty() {
            bail!("embed requires at least one text");
        }
        let body = serde_json::to_vec(&serde_json::json!({ "type": "embed", "text": text }))?;
        let session = self
            .client
            .do_exchange(tokio_stream::iter([metadata_message(&body)]))
            .await?;
        let mut inbound = session.into_inner();
        let response = inbound
            .message()
            .await?
            .context("bus closed before embed response")?
            .app_metadata;
        let parsed: serde_json::Value = serde_json::from_slice(&response)?;
        if parsed["type"] == "error" {
            bail!("bus embed rejected: {}", parsed["message"]);
        }
        serde_json::from_value(parsed["vectors"].clone()).context("embed response missing vectors")
    }
}

impl BusClient {
    /// Evolve a lane's latest intent via the bus's GEPA op (trust-region
    /// mutation + semantic-anchor drift), returning the evolution result.
    pub async fn evolve(
        &mut self,
        lane: &str,
        goal_vector: Vec<f32>,
        strength: f32,
        max_angle_deg: f32,
        anchor_beta: f32,
        seed: u64,
    ) -> Result<serde_json::Value> {
        self.evolve_request(
            lane,
            None,
            goal_vector,
            EvolveOptions {
                strength,
                max_angle_deg,
                anchor_beta,
                seed,
            },
        )
        .await
    }

    /// Evolve one exact intent/result candidate from a lane.
    pub async fn evolve_from(
        &mut self,
        lane: &str,
        parent_seq: u64,
        goal_vector: Vec<f32>,
        options: EvolveOptions,
    ) -> Result<serde_json::Value> {
        self.evolve_request(lane, Some(parent_seq), goal_vector, options)
            .await
    }

    async fn evolve_request(
        &mut self,
        lane: &str,
        parent_seq: Option<u64>,
        goal_vector: Vec<f32>,
        options: EvolveOptions,
    ) -> Result<serde_json::Value> {
        let mut request = serde_json::json!({
            "type": "evolve",
            "lane": lane,
            "goal_vector": goal_vector,
            "strength": options.strength,
            "max_angle_deg": options.max_angle_deg,
            "anchor_beta": options.anchor_beta,
            "seed": options.seed,
        });
        if let Some(parent_seq) = parent_seq {
            request["parent_seq"] = serde_json::json!(parent_seq);
        }
        let body = serde_json::to_vec(&request)?;
        let session = self
            .client
            .do_exchange(tokio_stream::iter([metadata_message(&body)]))
            .await?;
        let mut inbound = session.into_inner();
        let response = inbound
            .message()
            .await?
            .context("bus closed before evolve response")?
            .app_metadata;
        let parsed: serde_json::Value = serde_json::from_slice(&response)?;
        if parsed["type"] == "error" {
            bail!("bus evolve rejected: {}", parsed["message"]);
        }
        Ok(parsed)
    }
}
