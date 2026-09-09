//! Same-user local static artifact delivery with explicit, revision-bound authority.
use anyhow::{Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const MAX_BYTES: usize = 100 * 1024 * 1024;
const MAX_FILES: usize = 10_000;

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct State {
    active: Option<String>,
    previous: Option<String>,
    pending: Option<Pending>,
    history: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    action: String,
    revision: String,
    authorization: String,
    old_active: Option<String>,
    old_previous: Option<String>,
}

fn document(kind: &str, id: &str, details: Value) -> Value {
    json!({
        "@context": {"@vocab":"urn:leio:delivery:", "details":{"@id":"urn:leio:delivery:details","@type":"@json"}},
        "@id": format!("urn:leio:delivery:{kind}:{id}"),
        "@type": kind,
        "details": details
    })
}

fn safe_path(path: &Path) -> Result<()> {
    ensure!(
        !path.components().any(|p| matches!(p, Component::ParentDir)),
        "parent traversal forbidden"
    );
    let mut cursor = PathBuf::new();
    for part in path.components() {
        cursor.push(part);
        if let Ok(meta) = fs::symlink_metadata(&cursor) {
            ensure!(
                !meta.file_type().is_symlink(),
                "symlinks forbidden: {}",
                cursor.display()
            );
        }
    }
    Ok(())
}

fn root(target: &Path) -> Result<PathBuf> {
    boundary_path(target)?;
    fs::create_dir_all(target)?;
    let target = target.canonicalize()?;
    for name in ["releases", "state.jsonld", ".lock"] {
        safe_path(&target.join(name))?;
    }
    fs::create_dir_all(target.join("releases"))?;
    Ok(target)
}

// Canonicalize caller-selected directories before checking descendants. This
// permits OS aliases such as macOS /var while binding authority to the real path.
fn boundary_path(path: &Path) -> Result<()> {
    ensure!(
        !path
            .components()
            .any(|part| matches!(part, Component::ParentDir)),
        "parent traversal forbidden"
    );
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            !meta.file_type().is_symlink(),
            "source/target symlink forbidden"
        );
    }
    Ok(())
}

fn lock(target: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(target.join(".lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    safe_path(path)?;
    let tmp = path.with_extension("jsonld.tmp");
    safe_path(&tmp)?;
    let mut file = File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn sync_directories(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            sync_directories(&path)?;
        }
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn state(target: &Path) -> Result<State> {
    let path = target.join("state.jsonld");
    safe_path(&path)?;
    if !path.exists() {
        return Ok(State::default());
    }
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(serde_json::from_value(value["details"].clone())?)
}

fn save(target: &Path, value: &State) -> Result<()> {
    atomic_json(
        &target.join("state.jsonld"),
        &document(
            "Deployment",
            &hash(target.to_string_lossy().as_bytes()),
            serde_json::to_value(value)?,
        ),
    )
}

fn snapshot(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    fn walk(
        base: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
        total: &mut usize,
    ) -> Result<()> {
        safe_path(dir)?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            ensure!(!meta.file_type().is_symlink(), "source symlink forbidden");
            if meta.is_dir() {
                walk(base, &path, files, total)?;
            } else {
                ensure!(meta.is_file(), "only regular files supported");
                ensure!(meta.len() <= MAX_BYTES as u64, "artifact file too large");
                let bytes = fs::read(&path)?;
                *total += bytes.len();
                ensure!(
                    *total <= MAX_BYTES && files.len() < MAX_FILES,
                    "artifact size limit exceeded"
                );
                let relative = path
                    .strip_prefix(base)?
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("UTF-8 paths required"))?
                    .to_string();
                artifact_path(&relative)?;
                files.insert(relative, bytes);
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(dir, dir, &mut files, &mut 0)?;
    ensure!(
        files.contains_key("index.html"),
        "static artifact requires index.html"
    );
    Ok(files)
}

fn manifest(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, bytes)| (path.clone(), hash(bytes)))
        .collect()
}

fn artifact_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte)),
        "artifact paths must use URL-safe ASCII letters, digits, /, _, -, or ."
    );
    ensure!(
        path.split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "invalid artifact path"
    );
    ensure!(
        path.split('/').next() != Some("__leio"),
        "reserved __leio artifact path"
    );
    Ok(())
}

fn revision(files: &BTreeMap<String, String>) -> Result<String> {
    Ok(hash(&serde_json::to_vec(files)?))
}

fn valid_revision(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "invalid revision"
    );
    Ok(())
}

fn release(target: &Path, id: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    valid_revision(id)?;
    let dir = target.join("releases").join(id);
    safe_path(&dir)?;
    let files = snapshot(&dir.join("files"))?;
    let actual = manifest(&files);
    ensure!(revision(&actual)? == id, "release content tampered");
    ensure!(
        actual == release_manifest(target, id)?,
        "release manifest tampered"
    );
    Ok(files)
}

fn release_manifest(target: &Path, id: &str) -> Result<BTreeMap<String, String>> {
    valid_revision(id)?;
    let record = target.join("releases").join(id).join("artifact.jsonld");
    safe_path(&record)?;
    let doc: Value = serde_json::from_slice(&fs::read(record)?)?;
    let files: BTreeMap<String, String> = serde_json::from_value(doc["details"]["files"].clone())?;
    ensure!(
        doc["details"]["revision"] == id,
        "release identity tampered"
    );
    ensure!(revision(&files)? == id, "release manifest tampered");
    for path in files.keys() {
        artifact_path(path)?;
    }
    Ok(files)
}

fn authorization(target: &Path, action: &str, id: &str, current: &State) -> Result<String> {
    // Bind successful transition generation as well as revision: A -> B -> A
    // must not revive an approval issued during the first A deployment.
    let generation = current
        .history
        .iter()
        .filter(|event| event["status"] == "verified")
        .count();
    Ok(hash(&serde_json::to_vec(&(
        target,
        action,
        id,
        &current.active,
        generation,
    ))?))
}

/// Stage a directory containing actual build output. Does not claim to run a build.
pub fn prepare(source: &Path, target: &Path) -> Result<Value> {
    boundary_path(source)?;
    let source = source.canonicalize()?;
    let target = root(target)?;
    ensure!(
        !target.starts_with(&source) && !source.starts_with(&target),
        "source and deployment directories must be separate"
    );
    let _lock = lock(&target)?;
    let current = state(&target)?;
    ensure!(
        current.pending.is_none(),
        "pending promotion must be retried first"
    );
    let files = snapshot(&source)?;
    let entries = manifest(&files);
    let id = revision(&entries)?;
    let destination = target.join("releases").join(&id);
    if destination.exists() {
        release(&target, &id)?;
    } else {
        let staging = target.join("releases").join(format!(".staging-{id}"));
        safe_path(&staging)?;
        if staging.exists() {
            bail!(
                "interrupted staging exists at {}; inspect and remove before restaging",
                staging.display()
            );
        }
        fs::create_dir_all(staging.join("files"))?;
        for (path, bytes) in &files {
            let output = staging.join("files").join(path);
            fs::create_dir_all(output.parent().unwrap())?;
            let mut file = File::create(output)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        atomic_json(
            &staging.join("artifact.jsonld"),
            &document(
                "Artifact",
                &id,
                json!({"revision":id,"files":entries,"source":source,"created_at":chrono::Utc::now().to_rfc3339()}),
            ),
        )?;
        sync_directories(&staging)?;
        fs::rename(&staging, &destination)?;
        File::open(target.join("releases"))?.sync_all()?;
    }
    Ok(
        json!({"revision":id,"target":target,"authorization_digest":authorization(&target,"deploy",&id,&current)?,"files":entries}),
    )
}

pub fn inspect(target: &Path) -> Result<Value> {
    let target = root(target)?;
    let _lock = lock(&target)?;
    let current = state(&target)?;
    let rollback_authorization = current
        .previous
        .as_ref()
        .map(|id| authorization(&target, "rollback", id, &current))
        .transpose()?;
    Ok(
        json!({"target":target,"state":current,"rollback_authorization_digest":rollback_authorization}),
    )
}

fn request(address: SocketAddr, path: &str) -> Result<Vec<u8>> {
    ensure!(
        address.ip().is_loopback(),
        "only loopback runtime supported"
    );
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream
        .take((MAX_BYTES + 8192) as u64)
        .read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("invalid HTTP response"))?;
    ensure!(
        response.starts_with(b"HTTP/1.1 200 "),
        "runtime HTTP verification failed"
    );
    Ok(response[split + 4..].to_vec())
}

fn verify_revision(target: &Path, id: &str, address: SocketAddr) -> Result<Value> {
    let files = release(target, id)?;
    let identity: Value = serde_json::from_slice(&request(address, "/__leio/revision")?)?;
    ensure!(
        identity["revision"] == id
            && identity["target"] == hash(target.to_string_lossy().as_bytes()),
        "runtime revision/target mismatch"
    );
    for (path, expected) in &files {
        let actual = request(address, &format!("/{path}"))?;
        ensure!(
            hash(&actual) == hash(expected),
            "runtime artifact mismatch: {path}"
        );
    }
    ensure!(
        request(address, "/")? == files["index.html"],
        "runtime entry point mismatch"
    );
    let final_identity: Value = serde_json::from_slice(&request(address, "/__leio/revision")?)?;
    ensure!(
        final_identity == identity,
        "runtime changed during verification"
    );
    Ok(
        json!({"revision":id,"address":address.to_string(),"files_verified":files.len(),"verified_at":chrono::Utc::now().to_rfc3339(),"scope":"local_static_http"}),
    )
}

pub fn verify(target: &Path, address: SocketAddr) -> Result<Value> {
    let target = root(target)?;
    let _lock = lock(&target)?;
    let current = state(&target)?;
    let active = current
        .active
        .ok_or_else(|| anyhow::anyhow!("no active revision"))?;
    verify_revision(&target, &active, address)
}

fn promote(
    target: &Path,
    id: &str,
    approval: &str,
    address: SocketAddr,
    action: &str,
) -> Result<Value> {
    let target = root(target)?;
    let _lock = lock(&target)?;
    valid_revision(id)?;
    let mut current = state(&target)?;
    if let Some(pending) = &current.pending {
        ensure!(
            pending.action == action && pending.revision == id && pending.authorization == approval,
            "retry pending promotion with original authorization"
        );
    } else {
        ensure!(
            authorization(&target, action, id, &current)? == approval,
            "explicit authorization digest mismatch"
        );
        if action == "rollback" {
            ensure!(
                current.previous.as_deref() == Some(id),
                "rollback must select prior release"
            );
        }
        release(&target, id)?;
        if current.active.as_deref() == Some(id) {
            return verify_revision(&target, id, address);
        }
        current.pending = Some(Pending {
            action: action.into(),
            revision: id.into(),
            authorization: approval.into(),
            old_active: current.active.clone(),
            old_previous: current.previous.clone(),
        });
        current.previous = current.active.clone();
        current.active = Some(id.into());
        save(&target, &current)?;
    }
    match verify_revision(&target, id, address) {
        Ok(receipt) => {
            current
                .history
                .push(json!({"action":action,"status":"verified","receipt":receipt}));
            current.pending = None;
            save(&target, &current)?;
            Ok(receipt)
        }
        Err(error) => {
            let pending = current.pending.take().unwrap();
            current.active = pending.old_active;
            current.previous = pending.old_previous;
            current.history.push(json!({"action":action,"status":"failed","revision":id,"error":error.to_string(),"at":chrono::Utc::now().to_rfc3339()}));
            save(&target, &current)?;
            Err(error)
        }
    }
}

pub fn deploy(target: &Path, revision: &str, approval: &str, address: SocketAddr) -> Result<Value> {
    promote(target, revision, approval, address, "deploy")
}

pub fn rollback(target: &Path, approval: &str, address: SocketAddr) -> Result<Value> {
    let canonical = root(target)?;
    let current = state(&canonical)?;
    let id = current
        .pending
        .as_ref()
        .filter(|p| p.action == "rollback")
        .map(|p| p.revision.clone())
        .or(current.previous)
        .ok_or_else(|| anyhow::anyhow!("no previous release"))?;
    promote(&canonical, &id, approval, address, "rollback")
}

pub struct DeliveryServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
}
impl DeliveryServer {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    /// Bounded local diagnostics; never sent to unauthenticated HTTP clients.
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect()
    }
}
impl Drop for DeliveryServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn respond(target: &Path, stream: &mut TcpStream) -> Result<()> {
    // macOS inherits O_NONBLOCK from the listening socket. A connected client
    // may not have sent its request yet; use blocking I/O with bounded timeouts.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut input = Vec::new();
    let mut byte = [0u8];
    while input.len() < 8192 && !input.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        input.push(byte[0]);
    }
    let input = std::str::from_utf8(&input)?;
    let first = input.lines().next().unwrap_or_default();
    let parts = first.split_whitespace().collect::<Vec<_>>();
    ensure!(parts.len() == 3 && parts[0] == "GET", "GET required");
    let current = state(target)?;
    let id = current
        .active
        .ok_or_else(|| anyhow::anyhow!("no active release"))?;
    let files = release_manifest(target, &id)?;
    let path = parts[1];
    let (body, content_type) = if path == "/__leio/revision" {
        (
            serde_json::to_vec(
                &json!({"revision":id,"target":hash(target.to_string_lossy().as_bytes())}),
            )?,
            "application/json",
        )
    } else {
        let name: String = if path == "/" {
            "index.html".into()
        } else {
            ensure!(
                !path.contains('%') && !path.contains('?'),
                "encoded paths unsupported"
            );
            path.trim_start_matches('/').into()
        };
        artifact_path(&name)?;
        let expected = files
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("file not found"))?;
        let file = target.join("releases").join(&id).join("files").join(&name);
        safe_path(&file)?;
        ensure!(
            fs::metadata(&file)?.len() <= MAX_BYTES as u64,
            "file too large"
        );
        let body = fs::read(file)?;
        ensure!(hash(&body) == *expected, "release content tampered");
        let content_type = match Path::new(&name).extension().and_then(|ext| ext.to_str()) {
            Some("html" | "htm") => "text/html; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("js" | "mjs") => "text/javascript; charset=utf-8",
            Some("json" | "map") => "application/json",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("ico") => "image/x-icon",
            Some("wasm") => "application/wasm",
            Some("woff") => "font/woff",
            Some("woff2") => "font/woff2",
            Some("txt") => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        (body, content_type)
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    Ok(())
}

pub fn start_server(target: &Path, address: SocketAddr) -> Result<DeliveryServer> {
    ensure!(
        address.ip().is_loopback(),
        "only loopback binding supported"
    );
    let target = root(target)?;
    let listener = TcpListener::bind(address)?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let shutdown = stop.clone();
    let diagnostics = Arc::new(Mutex::new(VecDeque::new()));
    let errors = diagnostics.clone();
    let handle = thread::spawn(move || {
        while !shutdown.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if let Err(error) = respond(&target, &mut stream) {
                        let mut messages = errors.lock().unwrap_or_else(|error| error.into_inner());
                        if messages.len() == 16 {
                            messages.pop_front();
                        }
                        messages.push_back(format!("{error:#}").chars().take(512).collect());
                        let _=stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    Ok(DeliveryServer {
        address,
        stop,
        thread: Some(handle),
        diagnostics,
    })
}

pub fn serve(target: &Path, address: SocketAddr) -> Result<()> {
    let server = start_server(target, address)?;
    println!(
        "{}",
        json!({"address":server.address().to_string(),"scope":"local_static_http"})
    );
    loop {
        thread::park();
    }
}
