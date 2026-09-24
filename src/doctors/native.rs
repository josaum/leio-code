//! Explicitly trusted repository-owned Rust doctor executables. Discovery is data-only.
use super::{Doctor, utils};
use crate::model::{QueryEnvelope, RepoIndex, SCHEMA_VERSION};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub const MANIFEST: &str = ".leio-code/native-doctors.json";
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_BINARY: u64 = 512 * 1024 * 1024;
const MAX_REQUEST: u64 = 128 * 1024 * 1024;
struct RequestWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for RequestWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(std::io::Error::other("native doctor input exceeds limit"));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub name: String,
    pub description: String,
    pub suites: Vec<String>,
    #[serde(default)]
    pub covered_by: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub schema_version: u32,
    pub name: String,
    pub doctors: Vec<Definition>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trust {
    root: PathBuf,
    manifest_sha256: String,
    binary_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u32,
    request_id: String,
    root: PathBuf,
    index: RepoIndex,
    names: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    protocol: u32,
    request_id: String,
    results: BTreeMap<String, QueryEnvelope>,
}
fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn read_manifest(root: &Path) -> Result<Vec<u8>> {
    let path = root.join(MANIFEST);
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_MANIFEST,
        "native doctor manifest must be a bounded regular file"
    );
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.starts_with(root.canonicalize()?),
        "native doctor manifest escapes repository"
    );
    let bytes = fs::read(canonical)?;
    ensure!(
        bytes.len() <= MAX_MANIFEST as usize,
        "native doctor manifest exceeds limit"
    );
    Ok(bytes)
}
pub fn discover(root: &Path) -> Result<Option<Pack>> {
    if !root.join(MANIFEST).try_exists()? {
        return Ok(None);
    }
    let pack: Pack = serde_json::from_slice(&read_manifest(root)?)?;
    ensure!(
        pack.schema_version == 1 && slug(&pack.name),
        "unsupported native doctor pack"
    );
    ensure!(
        !pack.doctors.is_empty() && pack.doctors.len() <= 256,
        "native doctor count out of bounds"
    );
    let mut names = BTreeSet::new();
    let builtins = super::doctor_names();
    for d in &pack.doctors {
        ensure!(
            slug(&d.name)
                && !builtins.contains(&d.name.as_str())
                && !matches!(d.name.as_str(), "all" | "baseline" | "ci")
                && names.insert(&d.name),
            "duplicate, reserved or invalid native doctor name"
        );
        ensure!(
            d.description.len() <= 1024
                && !d.suites.is_empty()
                && d.suites
                    .iter()
                    .all(|s| matches!(s.as_str(), "all" | "baseline" | "ci")),
            "invalid native doctor metadata"
        );
    }
    for definition in &pack.doctors {
        if let Some(parent) = &definition.covered_by {
            ensure!(
                parent != &definition.name
                    && pack.doctors.iter().any(
                        |candidate| &candidate.name == parent && candidate.covered_by.is_none()
                    ),
                "invalid or cyclic native doctor composite"
            );
        }
    }
    Ok(Some(pack))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LEIO_DOCTOR_TRUST_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME unavailable for local doctor trust")?;
    Ok(PathBuf::from(home).join(".local/share/leio-code/doctor-packs"))
}
fn trust_path(root: &Path) -> Result<PathBuf> {
    Ok(state_dir()?.join(format!(
        "{}.json",
        digest(root.canonicalize()?.to_string_lossy().as_bytes())
    )))
}
fn open_binary(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Validate the opened object, without blocking on a FIFO or following a
        // substituted symlink between a path metadata check and open.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "doctor binary must be a regular file");
    ensure!(
        metadata.len() > 0 && metadata.len() <= MAX_BINARY,
        "doctor binary size out of bounds"
    );
    Ok(file)
}
fn copy_binary(source: fs::File, destination: &mut impl Write) -> Result<String> {
    // The opened file can grow after metadata validation. Bound the actual
    // stream as well, and keep memory use independent of executable size.
    let mut source = source.take(MAX_BINARY + 1);
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 16384];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= MAX_BINARY, "doctor binary size out of bounds");
        destination.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
    }
    ensure!(total > 0, "doctor binary size out of bounds");
    Ok(format!("{:x}", hash.finalize()))
}
/// Explicit host action: install the reviewed binary and bind it to this root and catalog.
pub fn trust(root: &Path, binary: &Path) -> Result<()> {
    discover(root)?.context("repository has no native doctor manifest")?;
    let root = root.canonicalize()?;
    let source = open_binary(binary)?;
    let dir = state_dir()?;
    fs::create_dir_all(&dir)?;
    ensure!(
        !fs::symlink_metadata(&dir)?.file_type().is_symlink(),
        "trust directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    let hash = copy_binary(source, &mut file)?;
    file.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o500))?;
    }
    file.persist(dir.join(&hash)).map_err(|e| e.error)?;
    let record = Trust {
        root: root.clone(),
        manifest_sha256: digest(&read_manifest(&root)?),
        binary_sha256: hash,
    };
    let mut record_file = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer(&mut record_file, &record)?;
    record_file.as_file().sync_all()?;
    record_file
        .persist(trust_path(&root)?)
        .map_err(|e| e.error)?;
    Ok(())
}
fn executable(root: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let trust:Trust=serde_json::from_slice(&fs::read(trust_path(&root)?).context("native doctors are not trusted; build and review the repository pack, then use trust-doctor-pack --binary PATH")?)?;
    ensure!(
        trust.root == root && trust.manifest_sha256 == digest(&read_manifest(&root)?),
        "native doctor catalog changed; review and trust it again"
    );
    ensure!(
        trust.binary_sha256.len() == 64
            && trust.binary_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid trusted binary digest"
    );
    let path = state_dir()?.join(&trust.binary_sha256);
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_BINARY,
        "invalid trusted binary"
    );
    ensure!(
        digest(&fs::read(&path)?) == trust.binary_sha256,
        "trusted native doctor binary changed"
    );
    Ok(path)
}
fn failure(error: impl std::fmt::Display) -> QueryEnvelope {
    QueryEnvelope {
        schema_version: SCHEMA_VERSION.into(),
        query_id: utils::query_id("native_doctor_error"),
        kind: "doctor".into(),
        summary: "repository doctor pack did not complete".into(),
        confidence: 0.0,
        entities: vec![],
        evidence: vec![],
        warnings: vec![error.to_string()],
        meta: Some(serde_json::json!({"state":"error","completed":false})),
        timing_ms: 0,
    }
}
pub fn run_one(name: &str, index: &RepoIndex, root: &Path) -> Option<QueryEnvelope> {
    match discover(root) {
        Ok(Some(pack)) if pack.doctors.iter().any(|d| d.name == name) => {
            Some(match invoke(index, root, &[name.to_owned()]) {
                Ok(mut values) => values
                    .remove(name)
                    .unwrap_or_else(|| failure("missing doctor response")),
                Err(e) => failure(e),
            })
        }
        Err(e) => Some(failure(e)),
        _ => None,
    }
}
pub fn run_suite(suite: &str, index: &RepoIndex, root: &Path) -> Vec<(String, QueryEnvelope)> {
    let pack = match discover(root) {
        Ok(Some(pack)) => pack,
        Ok(None) => return vec![],
        Err(e) => return vec![("native-pack-configuration".into(), failure(e))],
    };
    let selected: Vec<_> = pack
        .doctors
        .iter()
        .filter(|d| suite == "all" || d.suites.iter().any(|s| s == suite))
        .collect();
    let names: Vec<_> = selected
        .iter()
        .filter(|d| {
            !d.covered_by
                .as_ref()
                .is_some_and(|parent| selected.iter().any(|p| &p.name == parent))
        })
        .map(|d| d.name.clone())
        .collect();
    if names.is_empty() {
        return vec![];
    }
    // One large application must not spend every doctor's deadline in one
    // process and lose all completed results. Bound concurrency and each batch.
    use rayon::prelude::*;
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(4).build() {
        Ok(pool) => pool,
        Err(error) => {
            return names
                .into_iter()
                .map(|name| (name, failure(&error)))
                .collect();
        }
    };
    pool.install(|| {
        names
            .par_chunks(8)
            .map(|batch| match invoke(index, root, batch) {
                Ok(map) => map.into_iter().collect::<Vec<_>>(),
                Err(error) => batch
                    .iter()
                    .map(|name| (name.clone(), failure(&error)))
                    .collect(),
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}
fn invoke(
    index: &RepoIndex,
    root: &Path,
    names: &[String],
) -> Result<BTreeMap<String, QueryEnvelope>> {
    ensure!(
        std::env::var_os("LEIO_DISABLE_NATIVE_DOCTORS").is_none(),
        "native doctor execution is disabled by this host"
    );
    let binary = executable(root)?;
    let root = root.canonicalize()?;
    let request = Request {
        protocol: 1,
        request_id: utils::query_id("native"),
        root: root.clone(),
        index: index.clone(),
        names: names.to_vec(),
    };
    let mut input = tempfile::NamedTempFile::new()?;
    {
        // JSON serialization emits many small writes; buffer those syscalls and
        // enforce the budget as bytes are serialized, before growing the file.
        let mut writer = RequestWriter {
            inner: BufWriter::new(&mut input),
            remaining: MAX_REQUEST,
        };
        serde_json::to_writer(&mut writer, &request)?;
        writer.flush()?;
    }
    let mut command = Command::new(binary);
    command
        .env("LEIO_DOCTOR_HOST_COMMIT", crate::BUILD_COMMIT)
        .env("LEIO_DOCTOR_HOST_EXE", std::env::current_exe()?);
    command
        .args(["--request"])
        .arg(input.path())
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    struct Guard(std::process::Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut stdout = child
        .stdout
        .take()
        .context("native doctor stdout unavailable")?;
    let mut guard = Guard(child);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "could not bound native doctor output"
        );
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("native doctors currently require a Unix host");
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut bytes = Vec::new();
    let mut block = [0u8; 16384];
    let mut exit = None;
    loop {
        ensure!(Instant::now() < deadline, "native doctor deadline exceeded");
        match stdout.read(&mut block) {
            Ok(0) => {
                if exit.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(n) => {
                ensure!(
                    bytes.len() + n <= MAX_OUTPUT,
                    "native doctor output limit exceeded"
                );
                bytes.extend_from_slice(&block[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if exit.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
        if exit.is_none() {
            exit = guard.0.try_wait()?;
        }
    }
    ensure!(
        exit.is_some_and(|status| status.success()),
        "native doctor process failed"
    );
    let mut response: Response =
        serde_json::from_slice(&bytes).context("invalid native doctor response")?;
    ensure!(
        response.protocol == 1 && response.request_id == request.request_id,
        "native doctor response identity mismatch"
    );
    ensure!(
        response.results.keys().cloned().collect::<BTreeSet<_>>()
            == names.iter().cloned().collect(),
        "native doctor response names mismatch"
    );
    for (name, envelope) in &mut response.results {
        ensure!(
            envelope.schema_version == SCHEMA_VERSION && envelope.confidence.is_finite(),
            "invalid native doctor envelope"
        );
        // Legacy doctors omit completion metadata, so absence is not an error.
        // An explicit unsuccessful result must still fail warning-based gates,
        // even when the repository adapter forgot to emit a warning.
        if envelope.meta.as_ref().is_some_and(|meta| {
            meta.get("completed").and_then(serde_json::Value::as_bool) == Some(false)
                || matches!(
                    meta.get("state").and_then(serde_json::Value::as_str),
                    Some("error" | "failed" | "incomplete")
                )
        }) {
            envelope.warnings.push(format!(
                "native doctor `{name}` reported an error or incomplete result"
            ));
        }
    }
    Ok(response.results)
}
/// Shared JSON protocol adapter linked by repository-owned Rust executables.
pub fn serve(doctors: Vec<Box<dyn Doctor>>) -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 3 && args[1] == "--request",
        "expected --request PATH"
    );
    let file = fs::File::open(&args[2])?;
    ensure!(file.metadata()?.len() <= MAX_REQUEST, "request too large");
    let request: Request = serde_json::from_reader(BufReader::new(file.take(MAX_REQUEST + 1)))?;
    ensure!(
        request.protocol == 1 && request.root.is_absolute(),
        "unsupported native doctor request"
    );
    let mut results = BTreeMap::new();
    for name in &request.names {
        let doctor = doctors
            .iter()
            .find(|d| d.name() == name)
            .with_context(|| format!("unknown repository doctor {name}"))?;
        results.insert(name.clone(), doctor.run(&request.index, &request.root));
    }
    let response = Response {
        protocol: 1,
        request_id: request.request_id,
        results,
    };
    serde_json::to_writer(std::io::stdout().lock(), &response)?;
    Ok(())
}
