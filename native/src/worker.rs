//! Outbound worker actor with durable, fenced job outboxes and independent blocking log writers.
use crate::{
    client::{Client, Config},
    executor, state,
    workspace::{BlobStore, Manifest, ManifestEntry},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    future::Future,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_util::sync::CancellationToken;
pub(crate) mod control;
pub(crate) mod policy;
const DEFAULT_LOG_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    pub drain: bool,
    pub keep_failed: bool,
    pub cache_max_bytes: Option<u64>,
    pub max_log_bytes: u64,
    pub heartbeat_interval: Duration,
    pub reprobe_interval: Duration,
    pub update_interval: Duration,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    pub shutdown_result_grace: Duration,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            drain: false,
            keep_failed: false,
            cache_max_bytes: Some(DEFAULT_CACHE_BYTES),
            max_log_bytes: DEFAULT_LOG_BYTES,
            heartbeat_interval: Duration::from_secs(5),
            reprobe_interval: Duration::from_secs(15),
            update_interval: Duration::from_secs(30),
            reconnect_min: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
            shutdown_result_grace: Duration::from_secs(5),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerExit {
    Stopped,
    LocalStopped,
    Restart,
}
#[derive(Debug)]
pub struct RestartRequested;
impl std::fmt::Display for RestartRequested {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("worker requested supervised restart")
    }
}
impl std::error::Error for RestartRequested {}
#[derive(Clone)]
pub struct UpdateCheck {
    pub dir: PathBuf,
    pub client: Client,
    pub active_jobs: usize,
    pub pending_results: usize,
    pub update_only: bool,
    pub requested: bool,
    pub embedded: bool,
    /// Stop network preparation promptly, but finish and join any started filesystem mutation.
    pub cancel: CancellationToken,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateDecision {
    None,
    Reprobe,
    Restart,
}
pub type UpdateFuture = Pin<Box<dyn Future<Output = Result<UpdateDecision>> + Send>>;
pub type UpdateHook = Arc<dyn Fn(UpdateCheck) -> UpdateFuture + Send + Sync>;
#[derive(Clone)]
struct Active {
    assignment: Value,
    cancel: CancellationToken,
    phase: Arc<Mutex<String>>,
    seq: Arc<AtomicU64>,
    pins: Arc<Mutex<BTreeSet<String>>>,
}
type ActiveJobs = Arc<Mutex<HashMap<String, Active>>>;
struct WorkerLifetime {
    active: ActiveJobs,
    abandon: Arc<AtomicBool>,
    finish: CancellationToken,
}
impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.finish.cancel();
        self.abandon.store(true, Ordering::Release);
        for job in self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            job.cancel.cancel();
        }
    }
}
#[derive(Clone, Debug)]
enum Event {
    Frame(Value),
    Complete(String),
    Fatal(String),
    Info(Value),
    Update(UpdateDecision),
    Reconnect,
    LocalControl,
}
struct Endpoint {
    dir: PathBuf,
    config: Mutex<Value>,
    client: RwLock<Client>,
    relocating: tokio::sync::Mutex<()>,
}
impl Endpoint {
    fn config(&self) -> Value {
        self.config.lock().unwrap().clone()
    }
    fn client(&self) -> Client {
        self.client.read().unwrap().clone()
    }
    async fn adopt_name(&self, message: &Value) -> Result<()> {
        let Some(name) = message["name"].as_str() else {
            return Ok(());
        };
        let _lock = self.relocating.lock().await;
        let expected = self.config();
        ensure!(
            message
                .get("nodeId")
                .is_none_or(|node| *node == expected["nodeId"]),
            "controller sent a name for another worker"
        );
        if expected["name"] == name {
            return Ok(());
        }
        let name = name.to_owned();
        let path = self.dir.join("agent.json");
        let saved =
            tokio::task::spawn_blocking(move || persist_controller_name(&path, &expected, &name))
                .await??;
        let mut current = self.config.lock().unwrap();
        ensure!(
            current["nodeId"] == saved["nodeId"],
            "worker identity changed during rename"
        );
        current["name"] = saved["name"].clone();
        Ok(())
    }
    async fn relocate(&self, url: &str) -> Result<bool> {
        let _lock = self.relocating.lock().await;
        let mut config = self.config();
        let mut client = self.client();
        if !relocate_controller(&self.dir, &mut config, &mut client, url).await? {
            return Ok(false);
        }
        *self.config.lock().unwrap() = config;
        *self.client.write().unwrap() = client;
        Ok(true)
    }
}
fn persist_controller_name(path: &Path, expected: &Value, name: &str) -> Result<Value> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "controller sent an invalid worker name"
    );
    state::update_json(path, |saved| {
        for key in ["nodeId", "credential", "caPem", "clusterId"] {
            ensure!(
                saved[key] == expected[key],
                "worker identity changed during rename"
            );
        }
        saved["name"] = json!(name);
        Ok(())
    })
}
/// Probe using the existing pinned CA, persist first, and only then retarget the shared client.
pub async fn relocate_controller(
    dir: &Path,
    config: &mut Value,
    client: &mut Client,
    endpoint: &str,
) -> Result<bool> {
    let url = crate::client::normalize_url(endpoint)?;
    if url == client.config.url {
        return Ok(false);
    }
    let mut next_client = client.config.clone();
    next_client.url = url.clone();
    let probe = Client::new(next_client)?;
    let health = tokio::time::timeout(Duration::from_secs(10), probe.get("/v1/health"))
        .await
        .context("controller endpoint probe timed out")??;
    ensure!(health["ok"] == true, "controller endpoint is unhealthy");
    let path = dir.join("agent.json");
    let previous = config.clone();
    let old_url = client.config.url.clone();
    let next = tokio::task::spawn_blocking(move || {
        state::update_json(&path, |saved| {
            if saved.as_object().is_some_and(|saved| saved.is_empty()) {
                *saved = previous.clone();
            }
            for key in ["nodeId", "credential", "caPem"] {
                ensure!(
                    saved[key] == previous[key],
                    "agent identity changed during endpoint relocation"
                );
            }
            let mut fallbacks = Vec::<String>::new();
            for old in std::iter::once(old_url.as_str())
                .chain(saved["controllerUrl"].as_str())
                .chain(
                    saved["controllerFallbackUrls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
                .chain(
                    previous["controllerFallbackUrls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
            {
                if old != url && !fallbacks.iter().any(|previous| previous == old) {
                    fallbacks.push(old.to_owned());
                }
            }
            fallbacks.truncate(8);
            saved["controllerUrl"] = json!(url);
            saved["controllerFallbackUrls"] = json!(fallbacks);
            Ok(())
        })
    })
    .await??;
    *config = next;
    *client = probe;
    Ok(true)
}
fn client_from_state(config: &Value) -> Result<Client> {
    let ca = config["caPem"].as_str().context("missing pinned CA")?;
    let pin = crate::pki::fingerprint(ca)?;
    ensure!(
        config["caFingerprint"] == pin,
        "saved CA fingerprint does not match certificate"
    );
    Client::new(Config {
        version: 1,
        url: crate::client::normalize_url(
            config["controllerUrl"]
                .as_str()
                .context("missing controller URL")?,
        )?,
        ca_pem: ca.into(),
        ca_fingerprint: pin,
        token: config["credential"]
            .as_str()
            .context("missing agent credential")?
            .into(),
        token_name: None,
    })
}
pub fn resolved_act(dir: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("GRIDSTATOR_ACT") {
        return path.into();
    }
    if let Some(dir) = dir
        && let Ok(config) = state::read_json(&dir.join("agent.json"))
        && let Some(path) = config["managedActPath"].as_str()
        && Path::new(path).is_file()
    {
        return path.into();
    }
    "act".into()
}
fn build_metadata() -> Result<Value> {
    static HASH: OnceLock<String> = OnceLock::new();
    let hash = HASH.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| crate::release::hash_file(path).ok())
            .map(|(_, hash)| hash)
            .unwrap_or_default()
    });
    let installed = verified_managed_identity().ok();
    let managed = installed.is_some();
    let rollback = installed.and_then(|(_, state)| state.last_rollback);
    Ok(
        json!({"id":crate::BUILD_ID,"version":crate::VERSION,"sha256":if hash.is_empty(){Value::Null}else{json!(hash)},"target":crate::release::host_target()?,"managed":managed,"lastRollback":rollback,"runtime":"rust"}),
    )
}

fn verified_managed_identity()
-> Result<(crate::install::InstallStore, crate::install::InstallState)> {
    ensure!(
        std::env::var("GRIDSTATOR_SUPERVISED").as_deref() == Ok("1"),
        "not supervised"
    );
    ensure!(
        std::env::var("GRIDSTATOR_INSTALL_VERSION").as_deref() == Ok(crate::BUILD_ID),
        "launcher version differs from running build"
    );
    let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT").context("no managed root")?;
    let store = crate::install::InstallStore::new(PathBuf::from(root));
    let state = store.state()?.context("no managed state")?;
    // Activation precedes stopping the previous payload. That still-running, verified payload
    // remains managed while the new build is provisional; its reported ID remains its own ID.
    let previous_during_activation = state.previous.as_deref() == Some(crate::BUILD_ID)
        && state.pending.as_ref().is_some_and(|pending| {
            pending.previous == crate::BUILD_ID && pending.build_id == state.current
        });
    ensure!(
        state.current == crate::BUILD_ID || previous_during_activation,
        "running build is not active in managed state"
    );
    let installed = store.verify_installed(crate::BUILD_ID)?;
    ensure!(
        std::fs::canonicalize(std::env::current_exe()?)?
            == std::fs::canonicalize(installed.executable)?,
        "running executable is outside its verified managed version"
    );
    Ok((store, state))
}
/// Literal streaming masking holds only a suffix which could become a secret in the next chunk.
pub struct SecretMasker {
    secrets: Vec<String>,
    regex: Option<regex::Regex>,
    prefixes: Vec<Vec<usize>>,
    pending: BTreeMap<String, String>,
}
impl SecretMasker {
    pub fn new(mut secrets: Vec<String>) -> Result<Self> {
        secrets.retain(|s| !s.is_empty());
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        let regex = if secrets.is_empty() {
            None
        } else {
            Some(
                regex::RegexBuilder::new(
                    &secrets
                        .iter()
                        .map(|s| regex::escape(s))
                        .collect::<Vec<_>>()
                        .join("|"),
                )
                .size_limit(64 * 1024 * 1024)
                .build()?,
            )
        };
        let prefixes = secrets
            .iter()
            .map(|secret| {
                let bytes = secret.as_bytes();
                let mut prefix = vec![0; bytes.len()];
                let mut length = 0;
                for index in 1..bytes.len() {
                    while length > 0 && bytes[index] != bytes[length] {
                        length = prefix[length - 1];
                    }
                    if bytes[index] == bytes[length] {
                        length += 1;
                    }
                    prefix[index] = length;
                }
                prefix
            })
            .collect();
        Ok(Self {
            secrets,
            regex,
            prefixes,
            pending: BTreeMap::new(),
        })
    }
    pub fn redact(&self, text: &str) -> String {
        self.regex.as_ref().map_or_else(
            || text.to_owned(),
            |regex| regex.replace_all(text, "***").into_owned(),
        )
    }
    pub fn push(&mut self, stream: &str, chunk: &str) -> String {
        let mut text = self.pending.remove(stream).unwrap_or_default();
        text.push_str(chunk);
        let tail_start = self
            .regex
            .as_ref()
            .and_then(|regex| regex.find_iter(&text).last())
            .map_or(0, |matched| matched.end());
        let mut hold = 0;
        for (secret, prefix) in self.secrets.iter().zip(&self.prefixes) {
            let pattern = secret.as_bytes();
            let mut length = 0;
            for byte in &text.as_bytes()[tail_start..] {
                while length > 0 && *byte != pattern[length] {
                    length = prefix[length - 1];
                }
                if *byte == pattern[length] {
                    length += 1;
                }
                if length == pattern.len() {
                    length = prefix[length - 1];
                }
            }
            hold = hold.max(length);
        }
        let mut cut = text.len() - hold;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let output = self.redact(&text[..cut]);
        if cut < text.len() {
            self.pending.insert(stream.into(), text[cut..].into());
        }
        output
    }
    pub fn finish(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(stream, text)| (stream, self.redact(&text)))
            .collect()
    }
}
pub struct LogSpool {
    file: std::fs::File,
    seq: u64,
    data_bytes: u64,
    disk_bytes: u64,
    max_bytes: u64,
    truncated: bool,
    masker: SecretMasker,
}
impl LogSpool {
    pub fn open(path: &Path, secrets: Vec<String>, max_bytes: u64) -> Result<Self> {
        let old = replay_spool(path, 0, usize::MAX)?;
        let seq = old.last().and_then(|e| e["s"].as_u64()).unwrap_or(0);
        let data_bytes = old
            .iter()
            .filter_map(|e| e["d"].as_str())
            .map(|s| s.len() as u64)
            .sum();
        if let Some(parent) = path.parent() {
            state::private_dir(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        let disk_bytes = file.metadata()?.len();
        Ok(Self {
            file,
            seq,
            data_bytes,
            disk_bytes,
            max_bytes,
            truncated: false,
            masker: SecretMasker::new(secrets)?,
        })
    }
    pub fn last_seq(&self) -> u64 {
        self.seq
    }
    pub fn redact(&self, text: &str) -> String {
        self.masker.redact(text)
    }
    pub fn push(&mut self, stream: &str, text: &str) -> Result<Vec<Value>> {
        if self.truncated {
            return Ok(vec![]);
        }
        let text = self.masker.push(stream, text);
        self.append(stream, &text)
    }
    pub fn finish(&mut self) -> Result<Vec<Value>> {
        let mut entries = vec![];
        for (stream, text) in self.masker.finish() {
            entries.extend(self.append(&stream, &text)?);
        }
        Ok(entries)
    }
    fn append(&mut self, stream: &str, text: &str) -> Result<Vec<Value>> {
        if self.truncated || text.is_empty() {
            return Ok(vec![]);
        }
        let mut entries = vec![];
        let mut offset = 0;
        while offset < text.len() {
            let mut end = (offset + 64 * 1024).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let data = &text[offset..end];
            let entry = json!({"s":self.seq+1,"t":state::now(),"o":stream,"d":data});
            let encoded = serde_json::to_vec(&entry)?;
            if self.data_bytes + data.len() as u64 > self.max_bytes
                || self.disk_bytes + encoded.len() as u64 + 1
                    > self.max_bytes.saturating_mul(2).max(1024)
            {
                self.truncated = true;
                let entry = json!({"s":self.seq+1,"t":state::now(),"o":"system","d":format!("Output truncated: the job exceeded the {} MiB log limit\n",self.max_bytes/(1024*1024))});
                self.write(&entry)?;
                entries.push(entry);
                break;
            }
            self.data_bytes += data.len() as u64;
            self.write(&entry)?;
            entries.push(entry);
            offset = end;
        }
        Ok(entries)
    }
    fn write(&mut self, entry: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(entry)?;
        self.file.write_all(&bytes)?;
        self.file.write_all(b"\n")?;
        self.file.sync_data()?;
        self.disk_bytes += bytes.len() as u64 + 1;
        self.seq += 1;
        Ok(())
    }
}
pub fn replay_spool(path: &Path, after: u64, limit: usize) -> Result<Vec<Value>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut entries = vec![];
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry["s"].as_u64().is_some_and(|seq| seq > after) {
            entries.push(entry);
            if entries.len() >= limit {
                break;
            }
        }
    }
    Ok(entries)
}
fn valid_job_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}
fn result_files(dir: &Path) -> Result<Vec<Value>> {
    let mut results = vec![];
    for entry in std::fs::read_dir(dir.join("jobs"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name();
        let Some(id) = id.to_str().filter(|id| valid_job_id(id)) else {
            continue;
        };
        if let Ok(result) = state::read_json(&entry.path().join("result.json"))
            && result["jobId"] == id
        {
            results.push(result);
        }
    }
    results.sort_by(|a, b| a["jobId"].as_str().cmp(&b["jobId"].as_str()));
    Ok(results)
}
async fn pending_results(dir: &Path) -> Result<Vec<Value>> {
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || result_files(&dir)).await?
}
pub fn recover_interrupted(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir.join("jobs"))? {
        let path = entry?.path();
        if path.join("result.json").exists() {
            continue;
        }
        let Ok(assignment) = state::read_json(&path.join("assignment.json")) else {
            continue;
        };
        let Some(id) = assignment["jobId"].as_str().filter(|id| valid_job_id(id)) else {
            continue;
        };
        let seq = replay_spool(&path.join("logs.ndjson"), 0, usize::MAX)?
            .last()
            .and_then(|entry| entry["s"].as_u64())
            .unwrap_or(0);
        state::write_json(
            &path.join("result.json"),
            &json!({"jobId":id,"assignmentId":assignment["assignmentId"],"outcome":"lost","exitCode":null,"error":"agent restarted or lost track of the job","oomKilled":false,"finishedAt":state::now(),"logSeq":seq,"outputs":null,"artifacts":[]}),
        )?;
    }
    Ok(())
}

pub async fn node_info() -> Result<Value> {
    node_info_for(None).await
}

fn host_available_memory_bytes(system: &sysinfo::System) -> u64 {
    #[cfg(target_os = "macos")]
    if let Some(available) = darwin_available_memory_bytes(system.total_memory()) {
        return available;
    }
    system.available_memory().min(system.total_memory())
}

#[cfg(any(target_os = "macos", test))]
fn available_pages_bytes(free: u64, inactive: u64, page_size: u64, physical: u64) -> u64 {
    free.saturating_add(inactive)
        .saturating_mul(page_size)
        .min(physical)
}

#[cfg(target_os = "macos")]
fn darwin_available_memory_bytes(physical: u64) -> Option<u64> {
    unsafe extern "C" {
        fn mach_host_self() -> libc::host_t;
        fn host_page_size(
            host: libc::host_t,
            page_size: *mut libc::vm_size_t,
        ) -> libc::kern_return_t;
    }
    // Hold one send right for the process lifetime. Repeated heartbeats must not acquire
    // another mach_host_self right; the kernel releases this single right on process exit.
    static HOST: OnceLock<libc::host_t> = OnceLock::new();
    let host = *HOST.get_or_init(|| unsafe { mach_host_self() });
    let mut page_size = 0;
    let mut statistics = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // host_page_size reports the actual host page size under Rosetta as well. Both APIs
    // must succeed before any fields are read; sysinfo remains the conservative fallback.
    let success = unsafe {
        host_page_size(host, &mut page_size) == libc::KERN_SUCCESS
            && page_size > 0
            && libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                statistics.as_mut_ptr().cast(),
                &mut count,
            ) == libc::KERN_SUCCESS
            // Older kernels return a shorter revision than the current SDK/libc structure.
            // We only read the first three integer fields (free, active, inactive).
            && count >= 3
    };
    if !success {
        return None;
    }
    let statistics = unsafe { statistics.assume_init() };
    // A native host-available estimate, deliberately distinct from Node os.freemem's
    // free-only value: inactive pages are reclaimable. Do not subtract resident compressor
    // pages again or add overlapping speculative/purgeable counts. Docker VM capacity is
    // independently reported by executionCapacity and remains an admission bound.
    Some(available_pages_bytes(
        u64::from(statistics.free_count),
        u64::from(statistics.inactive_count),
        page_size as u64,
        physical,
    ))
}

async fn node_info_for(dir: Option<&Path>) -> Result<Value> {
    let host = tokio::task::spawn_blocking(|| {
        let system = sysinfo::System::new_all();
        (
            system.cpus().len().max(1) as u64,
            system.total_memory() / 1024 / 1024,
            host_available_memory_bytes(&system) / 1024 / 1024,
        )
    })
    .await?;
    let probe = |bin: String, args: Vec<&'static str>| async move {
        match tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(&bin)
                .kill_on_drop(true)
                .args(args)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                json!({"available":true,"version":String::from_utf8_lossy(&output.stdout).trim()})
            }
            Ok(Ok(output)) => {
                json!({"available":false,"error":String::from_utf8_lossy(&output.stderr).chars().take(1000).collect::<String>()})
            }
            _ => json!({"available":false,"error":format!("{bin} unavailable or timed out")}),
        }
    };
    let (docker, buildx, act, git) = tokio::join!(
        probe(
            executor::docker_binary(),
            vec!["version", "--format", "{{.Server.Version}}"]
        ),
        probe(executor::docker_binary(), vec!["buildx", "version"]),
        probe(
            resolved_act(dir).to_string_lossy().into_owned(),
            vec!["--version"]
        ),
        probe("git".into(), vec!["--version"])
    );
    let mut capacity = Value::Null;
    if docker["available"] == true
        && let Ok(Ok(output)) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(executor::docker_binary())
                .kill_on_drop(true)
                .args(["info", "--format", "{{json .}} "])
                .output(),
        )
        .await
        && output.status.success()
        && let Ok(info) = serde_json::from_slice::<Value>(&output.stdout)
        && let (Some(cpus), Some(bytes)) = (info["NCPU"].as_u64(), info["MemTotal"].as_u64())
    {
        capacity = json!({"cpus":cpus,"memoryTotalMb":bytes/1024/1024,"memoryAvailableMb":null,"diskFreeMb":null,"diskTotalMb":null});
    }
    let mut executors = vec![];
    if docker["available"] == true {
        executors.push("command");
        if buildx["available"] == true {
            executors.push("docker-build");
        }
        if act["available"] == true && git["available"] == true {
            executors.push("actions");
        }
    }
    if cfg!(target_os = "macos") && capacity.is_null() {
        executors.clear();
    }
    let mut build = tokio::task::spawn_blocking(build_metadata).await??;
    let saved = dir.and_then(|dir| state::read_json(&dir.join("agent.json")).ok());
    let override_path = std::env::var_os("GRIDSTATOR_ACT").is_some();
    let managed_path = saved
        .as_ref()
        .and_then(|config| config["managedActPath"].as_str());
    let managed_act =
        !override_path && managed_path.is_some_and(|path| resolved_act(dir) == Path::new(path));
    let act_version = act["version"].as_str().and_then(|version| {
        static VERSION: OnceLock<regex::Regex> = OnceLock::new();
        VERSION
            .get_or_init(|| {
                regex::Regex::new(r"\b(\d+\.\d+\.\d+)\b").expect("literal version regex")
            })
            .captures(version)
            .and_then(|captures| captures.get(1))
            .map(|matched| matched.as_str().to_owned())
    });
    build["tools"] = json!({"act":act_version,"managed":if managed_act{vec!["act"]}else{vec![]},"overrides":if override_path{vec!["act"]}else{vec![]}});
    let mut info = json!({"hostname":sysinfo::System::host_name().unwrap_or_else(||"worker".into()),"machineId":machine_id(),"platform":if cfg!(target_os="macos"){"darwin"}else{std::env::consts::OS},"arch":if cfg!(target_arch="x86_64"){"x64"}else{"arm64"},"osRelease":sysinfo::System::os_version().unwrap_or_default(),"cpus":host.0.max(1),"memoryTotalMb":host.1.max(16),"agentVersion":crate::VERSION,"docker":docker,"buildx":buildx,"act":act,"executors":executors,"build":build,"controllerUrl":saved.and_then(|config|config["controllerUrl"].as_str().map(str::to_owned)),"host":{"cpus":host.0,"memoryTotalMb":host.1,"memoryAvailableMb":host.2},"executionCapacity":capacity});
    if info["controllerUrl"].is_null() {
        info.as_object_mut()
            .expect("node info object")
            .remove("controllerUrl");
    }
    Ok(info)
}
pub(crate) fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|s| s.trim().into())
        .unwrap_or_else(|| {
            crate::workspace::digest_of(sysinfo::System::host_name().unwrap_or_default())
        })
}
pub async fn join(
    dir: &Path,
    url: &str,
    token: &str,
    pin: &str,
    name: Option<String>,
    settings: Option<Value>,
) -> Result<Value> {
    ensure!(
        !dir.join("agent.json").exists(),
        "agent already joined; use a new v2 data directory"
    );
    let url = crate::client::normalize_url(url)?;
    let ca = crate::pki::fetch_pinned_ca(&url, pin).await?;
    let fingerprint = crate::pki::fingerprint(&ca)?;
    let mut info = node_info_for(Some(dir)).await?;
    info["controllerUrl"] = json!(url);
    let config = Config {
        version: 1,
        url: url.clone(),
        ca_pem: ca.clone(),
        ca_fingerprint: fingerprint.clone(),
        token: token.into(),
        token_name: None,
    };
    let client = Client::new(config)?;
    let name = name.unwrap_or_else(|| info["hostname"].as_str().unwrap_or("worker").into());
    let mut body = json!({"name":name,"info":info});
    if let Some(settings) = settings.as_ref() {
        body["settings"] = settings.clone();
    }
    let joined = client.post("/v1/agents/register", body).await?;
    let mut state = json!({"version":2,"controllerUrl":url,"caPem":ca,"caFingerprint":fingerprint,"nodeId":joined["nodeId"],"name":joined["name"],"credential":joined["credential"]});
    if let Some(settings) = settings {
        state["settings"] = settings;
    }
    if joined["clusterId"].is_string() {
        state["clusterId"] = joined["clusterId"].clone();
    }
    state::write_json(&dir.join("agent.json"), &state)?;
    Ok(json!({"nodeId":joined["nodeId"],"name":joined["name"]}))
}

fn frame(job: &Active, kind: &str) -> Value {
    json!({"type":kind,"jobId":job.assignment["jobId"],"assignmentId":job.assignment["assignmentId"]})
}
fn log_entries(events: &broadcast::Sender<Event>, job: &Active, entries: Vec<Value>) {
    for entry in entries {
        job.seq
            .store(entry["s"].as_u64().unwrap_or(0), Ordering::Release);
        let mut message = frame(job, "job.log");
        message["entries"] = json!([entry]);
        let _ = events.send(Event::Frame(message));
    }
}
fn redact_value(value: &mut Value, masker: &SecretMasker) {
    match value {
        Value::String(text) => *text = masker.redact(text),
        Value::Array(items) => {
            for item in items {
                redact_value(item, masker);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                redact_value(item, masker);
            }
        }
        _ => {}
    }
}
async fn fetch_snapshot(
    endpoint: &Arc<Endpoint>,
    dir: &Path,
    id: &str,
    job: &Active,
) -> Result<(Manifest, BlobStore)> {
    ensure!(crate::protocol::valid_digest(id), "invalid snapshot ID");
    let first = endpoint.client();
    let fetch = |client: Client| async move {
        let snapshot = client
            .get(&format!("/v1/snapshots/{id}?manifest=true"))
            .await?;
        let manifest: Manifest = serde_json::from_value(snapshot["manifest"].clone())?;
        manifest.validate()?;
        ensure!(manifest.id()? == id, "snapshot manifest hash mismatch");
        let store = BlobStore::new(dir.join("blobs"));
        store.init()?;
        let mut digests = manifest
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ManifestEntry::File(file) => Some(file.digest.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        digests.insert(id.into());
        *job.pins.lock().unwrap() = digests;
        let bytes = manifest.serialize()?;
        let cache = store.clone();
        let digest = id.to_owned();
        tokio::task::spawn_blocking(move || cache.put_bytes(&digest, &bytes)).await??;
        let cache = store.clone();
        let digests = manifest
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ManifestEntry::File(file) => Some(file.digest.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let missing = tokio::task::spawn_blocking(move || cache.missing(&digests)).await??;
        crate::transfer::download_blobs(&client, &store, &missing).await?;
        Ok::<_, anyhow::Error>((manifest, store))
    };
    match fetch(first.clone()).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            let current = endpoint.client();
            if current.config.url != first.config.url {
                fetch(current).await
            } else {
                Err(error)
            }
        }
    }
}
async fn run_assignment(
    dir: PathBuf,
    endpoint: Arc<Endpoint>,
    events: broadcast::Sender<Event>,
    job: Active,
    abandon: Arc<AtomicBool>,
    options: WorkerOptions,
) -> Result<()> {
    let id = job.assignment["jobId"]
        .as_str()
        .context("job ID required")?
        .to_owned();
    let work = dir.join("jobs").join(&id);
    let assignment = job.assignment.clone();
    let target = work.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        state::private_dir(&target)?;
        state::write_json(&target.join("assignment.json"), &assignment)
    })
    .await??;
    let secrets = job.assignment["spec"]["secretEnv"]
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let masker = SecretMasker::new(secrets.clone())?;
    let (log_send, mut log_receive) = mpsc::channel::<(String, String)>(256);
    let logger_job = job.clone();
    let logger_events = events.clone();
    let log_path = work.join("logs.ndjson");
    let writer = tokio::task::spawn_blocking(move || -> Result<u64> {
        let result = (|| -> Result<u64> {
            let mut spool = LogSpool::open(&log_path, secrets, options.max_log_bytes)?;
            while let Some((stream, text)) = log_receive.blocking_recv() {
                if stream == "phase" {
                    if matches!(text.as_str(), "starting" | "running") {
                        *logger_job.phase.lock().unwrap() = text.clone();
                        let mut message = frame(&logger_job, "job.state");
                        message["phase"] = json!(text);
                        message["ts"] = json!(state::now());
                        let _ = logger_events.send(Event::Frame(message));
                    }
                    continue;
                }
                if stream == "outputs" {
                    if let Ok(mut outputs) = serde_json::from_str::<Value>(&text) {
                        redact_value(&mut outputs, &spool.masker);
                        let mut message = frame(&logger_job, "job.outputs");
                        message["outputs"] = outputs;
                        let _ = logger_events.send(Event::Frame(message));
                    }
                    continue;
                }
                if matches!(stream.as_str(), "stdout" | "stderr" | "system") {
                    log_entries(&logger_events, &logger_job, spool.push(&stream, &text)?);
                }
            }
            log_entries(&logger_events, &logger_job, spool.finish()?);
            Ok(spool.last_seq())
        })();
        if let Err(error) = &result {
            let _ = logger_events.send(Event::Fatal(format!(
                "durable job logging failed: {error:#}"
            )));
        }
        result
    });
    let _ = log_send
        .send(("system".into(), format!("Job {id} accepted by worker\n")))
        .await;
    let execution = async {
        let snapshot = match job.assignment["snapshotId"].as_str() {
            Some(id) => Some(
                tokio::select! {snapshot=fetch_snapshot(&endpoint,&dir,id,&job)=>snapshot?,_=job.cancel.cancelled()=>bail!("job cancelled during snapshot setup")},
            ),
            None => None,
        };
        let (manifest, blobs) = snapshot.map_or_else(
            || (None, BlobStore::new(dir.join("blobs"))),
            |(manifest, store)| (Some(manifest), store),
        );
        let config = endpoint.config();
        let context = executor::ExecutionContext {
            job_id: id.clone(),
            assignment_id: job.assignment["assignmentId"].as_str().unwrap_or("").into(),
            attempt: job.assignment["attempt"].as_u64().unwrap_or(1),
            node_id: config["nodeId"]
                .as_str()
                .context("node ID required")?
                .into(),
            node_name: config["name"].as_str().unwrap_or("worker").into(),
            spec: job.assignment["spec"].clone(),
            work_dir: work.clone(),
            manifest,
            blobs,
            keep_failed: options.keep_failed,
            act_bin: Some(resolved_act(Some(&dir))),
        };
        executor::execute(context, job.cancel.clone(), log_send.clone()).await
    };
    let mut result = match execution.await {
        Ok(result) => result,
        Err(error) => executor::ExecutionResult {
            outcome: if job.cancel.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            }
            .into(),
            exit_code: None,
            error: Some(format!("{error:#}")),
            oom_killed: false,
            outputs: Value::Null,
            artifacts: vec![],
        },
    };
    let mut artifacts = vec![];
    if result.artifacts.len() > 1000 {
        let _ = log_send
            .send((
                "system".into(),
                "Artifact collection limited to 1000 files\n".into(),
            ))
            .await;
    }
    for artifact in result.artifacts.iter().take(1000) {
        let client = endpoint.client();
        let upload = tokio::time::timeout(
            Duration::from_secs(60),
            crate::transfer::upload_artifact(&client, &artifact.path, &artifact.name),
        )
        .await
        .map_err(anyhow::Error::from)
        .and_then(|value| value);
        match upload {
            Ok(artifact) => artifacts.push(artifact),
            Err(error) => {
                let _ = log_send
                    .send((
                        "system".into(),
                        format!("Artifact {}: upload failed: {error:#}\n", artifact.name),
                    ))
                    .await;
                if job.assignment["spec"]["kind"] == "docker-build"
                    && job.assignment["spec"]["output"] == "tar"
                {
                    result.outcome = "failed".into();
                    result.error = Some(format!("Could not upload exported image: {error:#}"));
                }
            }
        }
    }
    if abandon.load(Ordering::Acquire) && result.outcome != "succeeded" {
        result.outcome = "lost".into();
        result.error = Some("the agent was shut down".into());
    }
    let _ = log_send
        .send(("system".into(), format!("Job {}\n", result.outcome)))
        .await;
    drop(log_send);
    let seq = writer.await.context("job log writer stopped")??;
    let mut result = json!({"jobId":id,"assignmentId":job.assignment["assignmentId"],"outcome":result.outcome,"exitCode":result.exit_code,"error":result.error,"oomKilled":result.oom_killed,"finishedAt":state::now(),"logSeq":seq,"outputs":result.outputs,"artifacts":artifacts});
    redact_value(&mut result, &masker);
    if let Some(error) = result["error"].as_str() {
        result["error"] = json!(crate::protocol::clip_text(error, 8192));
    }
    let path = work.join("result.json");
    tokio::task::spawn_blocking(move || state::write_json(&path, &result)).await??;
    let _ = events.send(Event::Complete(id));
    Ok(())
}
fn hello(
    info: &Value,
    active: &ActiveJobs,
    results: Vec<Value>,
    config: &Value,
    stopping: bool,
) -> Value {
    let jobs=active.lock().unwrap().values().map(|job|json!({"jobId":job.assignment["jobId"],"assignmentId":job.assignment["assignmentId"],"phase":*job.phase.lock().unwrap(),"logSeq":job.seq.load(Ordering::Acquire)})).collect::<Vec<_>>();
    let mut info = info.clone();
    info["controllerUrl"] = config["controllerUrl"].clone();
    info["acceptingWork"] = json!(!stopping);
    if stopping {
        info["executors"] = json!([]);
    }
    let mut message =
        json!({"type":"hello","protocolVersion":1,"info":info,"activeJobs":jobs,"results":results});
    if config["settings"]
        .as_object()
        .is_some_and(|settings| !settings.is_empty())
    {
        message["settings"] = config["settings"].clone();
    }
    message
}
fn local_hello(
    local: &control::Live,
    info: &Value,
    active: &ActiveJobs,
    results: Vec<Value>,
    config: &Value,
    stopping: bool,
) -> Value {
    let policy = local.policy.snapshot();
    let info = policy::advertise(info, &policy);
    let mut message = hello(
        &info,
        active,
        results,
        config,
        stopping || policy.paused || local.draining.load(Ordering::Acquire),
    );
    // Controller observations are not commands. Echoing a cached setting here would revert a
    // remote operator change made while this worker was disconnected.
    if config["settingsPending"] != true {
        message.as_object_mut().unwrap().remove("settings");
    }
    message
}
fn initial_admission_refresh(message: &Value, stopping: bool) -> bool {
    !stopping && message["info"]["acceptingWork"] == false
}

async fn send_frame<S>(sink: &Arc<tokio::sync::Mutex<S>>, message: Value) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        sink.lock()
            .await
            .send(Message::Text(message.to_string().into()))
            .await
    })
    .await
    .context("worker websocket send timed out")??;
    Ok(())
}
async fn replay_job<S>(
    sink: &Arc<tokio::sync::Mutex<S>>,
    dir: &Path,
    id: &str,
    after: u64,
    until: Option<u64>,
) -> Result<u64>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    if until.is_some_and(|until| after >= until) {
        return Ok(after);
    }
    let path = dir.join("jobs").join(id);
    let assignment = match state::read_json(&path.join("assignment.json")) {
        Ok(value) => value,
        Err(_) => return Ok(after),
    };
    let file = match tokio::fs::File::open(path.join("logs.ndjson")).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(after),
        Err(error) => return Err(error.into()),
    };
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(file).lines();
    let mut batch = Vec::new();
    let mut batch_bytes = 0;
    let mut sent = after;
    while let Some(line) = lines.next_line().await? {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(seq) = entry["s"].as_u64() else {
            continue;
        };
        if seq <= after {
            continue;
        }
        if until.is_some_and(|until| seq > until) {
            break;
        }
        batch_bytes += line.len();
        batch.push(entry);
        sent = seq;
        if batch.len() >= 64 || batch_bytes >= 256 * 1024 {
            send_frame(sink,json!({"type":"job.log","jobId":id,"assignmentId":assignment["assignmentId"],"entries":std::mem::take(&mut batch)})).await?;
            batch_bytes = 0;
        }
    }
    if !batch.is_empty() {
        send_frame(sink,json!({"type":"job.log","jobId":id,"assignmentId":assignment["assignmentId"],"entries":batch})).await?;
    }
    Ok(sent)
}
async fn send_result<S>(
    sink: &Arc<tokio::sync::Mutex<S>>,
    dir: &Path,
    id: &str,
    delivered: &mut HashMap<String, u64>,
) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let result = match state::read_json(&dir.join("jobs").join(id).join("result.json")) {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };
    let required = result["logSeq"].as_u64().unwrap_or(0);
    let sent = replay_job(
        sink,
        dir,
        id,
        *delivered.get(id).unwrap_or(&0),
        Some(required),
    )
    .await?;
    ensure!(sent >= required, "durable result has missing log entries");
    delivered.insert(id.into(), sent);
    send_frame(sink, json!({"type":"job.result","result":result})).await
}
async fn acknowledge(dir: &Path, id: &str) -> Result<()> {
    if !valid_job_id(id) {
        return Ok(());
    }
    let path = dir.join("jobs").join(id);
    let history_dir = dir.to_owned();
    let history_id = id.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if path.join("result.json").is_file() {
            crate::local_worker::archive_job(&history_dir, &history_id)?;
            std::fs::remove_dir_all(path)?;
        }
        Ok(())
    })
    .await?
}
fn evict_cache(dir: &Path, active: &ActiveJobs, maximum: Option<u64>) {
    let Some(maximum) = maximum else {
        return;
    };
    let pins = active
        .lock()
        .unwrap()
        .values()
        .flat_map(|job| job.pins.lock().unwrap().iter().cloned().collect::<Vec<_>>())
        .collect::<BTreeSet<_>>();
    let cache = BlobStore::new(dir.join("blobs"));
    tokio::task::spawn_blocking(move || {
        if let Err(error) = cache.evict_lru(maximum, &pins) {
            eprintln!("worker cache eviction: {error}");
        }
    });
}
async fn sample_metrics(dir: &Path, previous: Arc<Mutex<sysinfo::System>>) -> Result<Value> {
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move||{let mut system=previous.lock().unwrap();system.refresh_cpu_usage();system.refresh_memory();let disks=sysinfo::Disks::new_with_refreshed_list();let disk=disks.iter().filter(|disk|dir.starts_with(disk.mount_point())).max_by_key(|disk|disk.mount_point().as_os_str().len());json!({"ts":state::now(),"cpuPercent":system.global_cpu_usage(),"memoryAvailableMb":host_available_memory_bytes(&system)/1024/1024,"diskFreeMb":disk.map_or(0,|disk|disk.available_space()/1024/1024),"diskTotalMb":disk.map_or(0,|disk|disk.total_space()/1024/1024),"load1":sysinfo::System::load_average().one,"diskScope":"agent-data-host","executionDisk":null})}).await.map_err(Into::into)
}

pub async fn start(dir: PathBuf, drain: bool) -> Result<()> {
    start_options(dir, drain, false, Some(DEFAULT_CACHE_BYTES)).await
}
pub async fn start_options(
    dir: PathBuf,
    drain: bool,
    keep_failed: bool,
    cache_max_bytes: Option<u64>,
) -> Result<()> {
    let stop = CancellationToken::new();
    let signal = stop.clone();
    let listener = tokio::spawn(async move {
        state::shutdown_signal().await;
        signal.cancel();
    });
    let mut options = WorkerOptions {
        drain,
        keep_failed,
        cache_max_bytes: Some(cache_max_bytes.unwrap_or(DEFAULT_CACHE_BYTES)),
        ..Default::default()
    };
    if let Ok(config) = state::read_json(&dir.join("agent.json"))
        && let Some(limit) = config["maxLogBytes"].as_u64()
    {
        options.max_log_bytes = limit;
    }
    let hook: UpdateHook = Arc::new(|check| Box::pin(crate::updates::worker_update(check)));
    let result = run_with_shutdown(dir, options, stop, Some(hook)).await;
    listener.abort();
    match result? {
        WorkerExit::Stopped | WorkerExit::LocalStopped => Ok(()),
        WorkerExit::Restart => Err(RestartRequested.into()),
    }
}
struct Lifecycle {
    stopping: bool,
    restart: bool,
    fatal: Option<String>,
    quiet_since: Option<Instant>,
    local: bool,
}
impl Lifecycle {
    fn stop(&mut self, active: &ActiveJobs, abandon: &Arc<AtomicBool>, drain: bool) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if !drain {
            abandon.store(true, Ordering::Release);
            for task in active.lock().unwrap().values() {
                task.cancel.cancel();
            }
        }
    }
    async fn exit(
        &mut self,
        dir: &Path,
        active: &ActiveJobs,
        options: &WorkerOptions,
    ) -> Result<Option<WorkerExit>> {
        if !self.stopping && !self.restart {
            return Ok(None);
        }
        if !active.lock().unwrap().is_empty() {
            self.quiet_since = None;
            return Ok(None);
        }
        let pending = pending_results(dir).await?.len();
        if pending > 0 && !self.local {
            let since = self.quiet_since.get_or_insert_with(Instant::now);
            if options.drain || self.restart || since.elapsed() < options.shutdown_result_grace {
                return Ok(None);
            }
        }
        if let Some(error) = &self.fatal {
            bail!("{error}");
        }
        Ok(Some(if self.restart {
            WorkerExit::Restart
        } else if self.local {
            WorkerExit::LocalStopped
        } else {
            WorkerExit::Stopped
        }))
    }
}
/// Controlled entrypoint for embedding/tests. Socket and heartbeat remain alive during a drain;
/// ordinary shutdown records interrupted work as lost and leaves unacknowledged results on disk.
pub async fn run_with_shutdown(
    dir: PathBuf,
    options: WorkerOptions,
    stop: CancellationToken,
    hook: Option<UpdateHook>,
) -> Result<WorkerExit> {
    // The local bridge resolves owned data directories before deriving their control socket.
    // macOS may expose the same directory through /var and /private/var; use one identity here.
    let dir = crate::local_worker::owned_directory(&dir)?;
    ensure!(
        !options.heartbeat_interval.is_zero()
            && !options.reprobe_interval.is_zero()
            && !options.update_interval.is_zero(),
        "worker intervals must be positive"
    );
    let config = state::read_json(&dir.join("agent.json"))?;
    ensure!(config["version"] == 2, "refusing non-v2 agent state");
    let client = client_from_state(&config)?;
    state::private_dir(&dir.join("jobs"))?;
    state::private_dir(&dir.join("blobs"))?;
    // Claim the data directory before orphan cleanup: a second foreground process must never
    // remove the running service's containers or compete for its durable result outbox.
    let lock_path = dir.join("agent.lock");
    let _instance = Arc::new(
        tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(lock_path)?;
            // Offline bridge requests briefly claim this lock while reading or updating local
            // state. A worker launched at that instant should wait for the request to finish.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match fs2::FileExt::try_lock_exclusive(&file) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => {
                        return Err(error).context("another worker owns this data directory");
                    }
                }
            }
            Ok(file)
        })
        .await??,
    );
    let endpoint = Arc::new(Endpoint {
        dir: dir.clone(),
        config: Mutex::new(config),
        client: RwLock::new(client),
        relocating: tokio::sync::Mutex::new(()),
    });
    let active: ActiveJobs = Arc::new(Mutex::new(HashMap::new()));
    let (events, _) = broadcast::channel::<Event>(1024);
    let mut receiver = events.subscribe();
    let abandon = Arc::new(AtomicBool::new(false));
    let stopping = Arc::new(AtomicBool::new(false));
    let holding = Arc::new(AtomicBool::new(false));
    let update_only = Arc::new(AtomicBool::new(false));
    let runtime_stop = CancellationToken::new();
    let _lifetime = WorkerLifetime {
        active: active.clone(),
        abandon: abandon.clone(),
        finish: runtime_stop.clone(),
    };
    let local = Arc::new(control::Live::new(
        dir.clone(),
        active.clone(),
        endpoint.clone(),
        events.clone(),
    )?);
    let _control = control::serve(local.clone(), runtime_stop.clone())?;
    let recovering = dir.clone();
    tokio::task::spawn_blocking(move || recover_interrupted(&recovering)).await??;
    let mut info = node_info_for(Some(&dir)).await?;
    *local.info.write().unwrap() = info.clone();
    let node_id = endpoint.config()["nodeId"]
        .as_str()
        .context("missing node ID")?
        .to_owned();
    let mut cleaned = false;
    if info["docker"]["available"] == true {
        if let Err(error) = executor::cleanup_managed(&node_id, &HashSet::new()).await {
            eprintln!("worker startup cleanup: {error:#}");
        } else {
            cleaned = true;
        }
    }
    evict_cache(&dir, &active, options.cache_max_bytes);
    let (updates_send, mut updates_receive) = mpsc::channel::<bool>(8);
    let updater = hook.map(|hook| {
        let active = active.clone();
        let endpoint = endpoint.clone();
        let dir = dir.clone();
        let stopping = stopping.clone();
        let holding = holding.clone();
        let update_only = update_only.clone();
        let finish = runtime_stop.clone();
        let events = events.clone();
        let interval = options.update_interval;
        let instance = _instance.clone();
        tokio::spawn(async move {
            // Also retain ownership if the embedding caller drops the actor future. The update
            // hook may already be joining a blocking install or activation that cannot be aborted.
            let _instance = instance;
            let mut timer = tokio::time::interval(interval);
            loop {
                let requested = tokio::select! {
                    biased;
                    _ = finish.cancelled() => break,
                    _ = timer.tick() => false,
                    request = updates_receive.recv() => match request {
                        Some(request) => request,
                        None => break,
                    },
                };
                if stopping.load(Ordering::Acquire) || finish.is_cancelled() {
                    continue;
                }
                // Hold admission before reading active jobs; assignment rechecks under the same
                // active-map mutex, preventing a stale zero while accepting a new job.
                holding.store(true, Ordering::Release);
                let active_jobs = active.lock().unwrap().len();
                let pending_results = match pending_results(&dir).await {
                    Ok(results) => results.len(),
                    Err(error) => {
                        eprintln!("worker update outbox scan: {error}");
                        holding.store(false, Ordering::Release);
                        continue;
                    }
                };
                let check = UpdateCheck {
                    dir: dir.clone(),
                    client: endpoint.client(),
                    active_jobs,
                    pending_results,
                    update_only: update_only.load(Ordering::Acquire),
                    requested,
                    embedded: crate::local_worker::embedded(&dir),
                    cancel: finish.clone(),
                };
                let decision = match hook(check).await {
                    Ok(decision) => decision,
                    Err(error) => {
                        eprintln!("worker update: {error:#}");
                        UpdateDecision::None
                    }
                };
                holding.store(decision != UpdateDecision::None, Ordering::Release);
                let _ = events.send(Event::Update(decision));
            }
        })
    });
    let probe_task = {
        let finish = runtime_stop.clone();
        let events = events.clone();
        let dir = dir.clone();
        let interval = options.reprobe_interval;
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            timer.tick().await;
            loop {
                tokio::select! {_=finish.cancelled()=>break,_=timer.tick()=>{match node_info_for(Some(&dir)).await{Ok(info)=>{let _=events.send(Event::Info(info));},Err(error)=>eprintln!("worker capability probe: {error:#}")}}}
            }
        })
    };
    let mut lifecycle = Lifecycle {
        stopping: false,
        restart: false,
        fatal: None,
        quiet_since: None,
        local: false,
    };
    let mut backoff = options.reconnect_min;
    let mut failures = 0;
    let sampler = Arc::new(Mutex::new(sysinfo::System::new_all()));
    let mut retry = tokio::time::interval(Duration::from_millis(100));
    let result=async{loop{
 if stop.is_cancelled()&&!lifecycle.stopping{lifecycle.stop(&active,&abandon,options.drain);stopping.store(true,Ordering::Release);}
 if local.draining.load(Ordering::Acquire) { lifecycle.stopping=true; lifecycle.local=true; stopping.store(true,Ordering::Release); }
 if let Some(exit)=lifecycle.exit(&dir,&active,&options).await?{break Ok(exit);}
 let current=endpoint.client();let mut request=format!("{}/v1/agent/ws",current.config.url.replacen("https://","wss://",1)).into_client_request()?;request.headers_mut().insert("authorization",format!("Bearer {}",current.config.token).parse()?);
 let connection=if update_only.load(Ordering::Acquire){None}else{tokio::select!{
 connection=tokio::time::timeout(Duration::from_secs(12),tokio_tungstenite::connect_async_tls_with_config(request,None,false,Some(tokio_tungstenite::Connector::Rustls(crate::pki::websocket_tls(&current.config.ca_pem)?))))=>Some(connection.map_err(anyhow::Error::from).and_then(|value|value.map_err(Into::into))),
 _=stop.cancelled(),if !lifecycle.stopping=>{lifecycle.stop(&active,&abandon,options.drain);stopping.store(true,Ordering::Release);continue;}
 _=local.shutdown.cancelled(),if !lifecycle.stopping=>{lifecycle.stopping=true;lifecycle.local=true;stopping.store(true,Ordering::Release);continue;}
 }};
 if let Some(Ok((socket,_)))=connection{
 failures=0;backoff=options.reconnect_min;let(socket_sink,mut stream)=socket.split();let sink=Arc::new(tokio::sync::Mutex::new(socket_sink));let mut delivered=HashMap::<String,u64>::new();let mut welcomed=false;
 let initial_hello=local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),lifecycle.stopping||holding.load(Ordering::Acquire));
 let mut refresh_after_welcome=initial_admission_refresh(&initial_hello,lifecycle.stopping);
 if send_frame(&sink,initial_hello).await.is_err(){continue;}
 let heartbeat_ready=Arc::new(AtomicBool::new(false));
 let heartbeat_task={let ready=heartbeat_ready.clone();let sink=sink.clone();let active=active.clone();let sampler=sampler.clone();let dir=dir.clone();let events=events.clone();let interval=options.heartbeat_interval;tokio::spawn(async move{let mut timer=tokio::time::interval(interval);loop{timer.tick().await;if !ready.load(Ordering::Acquire){continue;}let metrics=match sample_metrics(&dir,sampler.clone()).await{Ok(metrics)=>metrics,Err(error)=>{eprintln!("worker heartbeat sample: {error}");continue;}};let jobs=active.lock().unwrap().keys().cloned().collect::<Vec<_>>();if send_frame(&sink,json!({"type":"heartbeat","metrics":metrics,"activeJobs":jobs})).await.is_err(){let _=events.send(Event::Reconnect);break;}}})};
 let mut finish_tick=tokio::time::interval(Duration::from_millis(100));
 let session:Result<()>=async{loop{tokio::select!{
 _=stop.cancelled(),if !lifecycle.stopping=>{lifecycle.stop(&active,&abandon,options.drain);stopping.store(true,Ordering::Release);send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),true)).await?;}
 _=local.shutdown.cancelled(),if !lifecycle.stopping=>{lifecycle.stopping=true;lifecycle.local=true;stopping.store(true,Ordering::Release);send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),true)).await?;}
 _=finish_tick.tick()=>{if lifecycle.exit(&dir,&active,&options).await?.is_some(){break;}},
 incoming=stream.next()=>{
 let message=match incoming{Some(Ok(Message::Text(text)))=>serde_json::from_str::<Value>(&text)?,Some(Ok(Message::Ping(data)))=>{sink.lock().await.send(Message::Pong(data)).await?;continue;},Some(Ok(Message::Pong(_)))=>continue,_=>break};
 match message["type"].as_str(){
 Some("welcome")=>{
 local.connected.store(true,Ordering::Release);
 if message["settings"].is_object(){*local.controller.lock().unwrap()=message["settings"].clone();}
 if endpoint.config()["settingsPending"]==true {
   let saved=endpoint.config();let path=dir.join("agent.json");
   tokio::task::spawn_blocking(move||state::update_json(&path,|current|{if current["nodeId"]==saved["nodeId"]&&current["settings"]==saved["settings"]{current["settingsPending"]=json!(false);}Ok(())})).await??;
   endpoint.config.lock().unwrap()["settingsPending"]=json!(false);
 }
 if let Some(cluster)=message["clusterId"].as_str(){
   let saved=endpoint.config();
   ensure!(saved["clusterId"].is_null()||saved["clusterId"]==cluster,"controller cluster identity changed");
   if saved["clusterId"].is_null(){let path=dir.join("agent.json");let cluster=cluster.to_owned();let node=saved["nodeId"].clone();let copy=cluster.clone();tokio::task::spawn_blocking(move||state::update_json(&path,|current|{ensure!(current["nodeId"]==node,"worker identity changed");current["clusterId"]=json!(copy);Ok(())})).await??;endpoint.config.lock().unwrap()["clusterId"]=json!(cluster);}
 }
 endpoint.adopt_name(&message).await?;
 heartbeat_ready.store(true,Ordering::Release);
 for resume in message["resume"].as_array().into_iter().flatten(){let Some(id)=resume["jobId"].as_str().filter(|id|valid_job_id(id))else{continue;};let after=resume["logSeq"].as_u64().unwrap_or(0);let sent=replay_job(&sink,&dir,id,after,None).await?;delivered.insert(id.into(),sent);}
 for result in pending_results(&dir).await?{if let Some(id)=result["jobId"].as_str(){send_result(&sink,&dir,id,&mut delivered).await?;}}
 welcomed=true;
 if refresh_after_welcome&&!holding.load(Ordering::Acquire)&&!lifecycle.stopping{refresh_after_welcome=false;send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),false)).await?;}
 let confirmation_owner = _instance.clone();
 tokio::task::spawn_blocking(move || -> Result<()> {
   let _owner = confirmation_owner;
   if let Ok((store, _)) = verified_managed_identity() {
     let confirmation = store.confirm_running_payload()?;
     if let Some(error) = confirmation.refresh_error {
       eprintln!("worker supervisor refresh: {error}");
     }
   }
   Ok(())
 }).await??;
 eprintln!("Agent {} connected",endpoint.config()["name"].as_str().unwrap_or("worker"));
 if let Some(url)=message["controllerUrl"].as_str(){match endpoint.relocate(url).await{Ok(true)=>{break;},Ok(false)=>{},Err(error)=>eprintln!("controller relocation retained current endpoint: {error:#}")}}
 }
 Some("job.assign")=>{
 let Some(id)=message["jobId"].as_str().filter(|id|valid_job_id(id)).map(str::to_owned)else{continue;};
 let refusal={let jobs=active.lock().unwrap();if stop.is_cancelled()||lifecycle.stopping||lifecycle.restart||holding.load(Ordering::Acquire){Some("agent is draining or checking an update")}else if let Some(existing)=jobs.get(&id){if existing.assignment["assignmentId"]==message["assignmentId"]{None}else{Some("job already has another active assignment")}}else if state::read_json(&dir.join("jobs").join(&id).join("result.json")).is_ok(){Some("job has a durable unacknowledged result")}else{None}};
 if let Some(reason)=refusal{send_frame(&sink,json!({"type":"job.rejected","jobId":id,"assignmentId":message["assignmentId"],"reason":reason})).await?;continue;}
 if active.lock().unwrap().contains_key(&id){continue;}
 let kind=message["spec"]["kind"].as_str().unwrap_or("command");if !info["executors"].as_array().is_some_and(|executors|executors.iter().any(|executor|executor==kind)){send_frame(&sink,json!({"type":"job.rejected","jobId":id,"assignmentId":message["assignmentId"],"reason":"executor unavailable; waiting for worker tools"})).await?;continue;}
 let task=Active{assignment:message.clone(),cancel:CancellationToken::new(),phase:Arc::new(Mutex::new("starting".into())),seq:Arc::new(AtomicU64::new(0)),pins:Arc::new(Mutex::new(BTreeSet::new()))};
 if let Err(reason)=local.admit(task.clone(),stop.is_cancelled()||holding.load(Ordering::Acquire)){send_frame(&sink,json!({"type":"job.rejected","jobId":id,"assignmentId":message["assignmentId"],"reason":reason.to_string()})).await?;continue;}

 let mut starting=frame(&task,"job.state");starting["phase"]=json!("starting");starting["ts"]=json!(state::now());send_frame(&sink,starting).await?;
 let(dir,endpoint,events,active,abandon,options,instance)=(dir.clone(),endpoint.clone(),events.clone(),active.clone(),abandon.clone(),options.clone(),_instance.clone());
 tokio::spawn(async move{let _instance=instance;if let Err(error)=run_assignment(dir.clone(),endpoint,events.clone(),task,abandon,options.clone()).await{let _=events.send(Event::Fatal(format!("could not preserve job {id} result: {error:#}")));}active.lock().unwrap().remove(&id);evict_cache(&dir,&active,options.cache_max_bytes);});
 }
 Some("job.cancel")=>{if let Some(job)=active.lock().unwrap().get(message["jobId"].as_str().unwrap_or("")){job.cancel.cancel();}}
 Some("job.ack")=>{if let Some(id)=message["jobId"].as_str(){acknowledge(&dir,id).await?;delivered.remove(id);}}
 Some("node.settings")=>{endpoint.adopt_name(&message).await?;if message["settings"].is_object(){*local.controller.lock().unwrap()=message["settings"].clone();}let config=endpoint.config();eprintln!("Worker {} settings updated",config["name"].as_str().unwrap_or("worker"));}
 Some("update.check")=>{let _=updates_send.try_send(true);}
 Some("error")if message["fatal"]==true=>{if message["code"]=="PROTOCOL_MISMATCH"&&updater.is_some()&&info["build"]["managed"]==true{update_only.store(true,Ordering::Release);let _=updates_send.try_send(true);break;}lifecycle.fatal=Some(message["message"].as_str().unwrap_or("controller rejected worker").into());lifecycle.stop(&active,&abandon,false);stopping.store(true,Ordering::Release);break;}
 _=>{}
 }
 },
 event=receiver.recv()=>match event{
 Ok(Event::Frame(message))if welcomed=>{if message["type"]=="job.log"{let Some(id)=message["jobId"].as_str()else{continue;};let target=message["entries"].as_array().and_then(|entries|entries.last()).and_then(|entry|entry["s"].as_u64()).unwrap_or(0);let after=*delivered.get(id).unwrap_or(&0);if target>after{let entries=message["entries"].as_array().unwrap();let fresh=entries.iter().filter(|entry|entry["s"].as_u64().is_some_and(|seq|seq>after)).cloned().collect::<Vec<_>>();if fresh.first().and_then(|entry|entry["s"].as_u64())==Some(after+1){send_frame(&sink,json!({"type":"job.log","jobId":id,"assignmentId":message["assignmentId"],"entries":fresh})).await?;delivered.insert(id.into(),target);}else{let sent=replay_job(&sink,&dir,id,after,Some(target)).await?;delivered.insert(id.into(),sent);}}}else{send_frame(&sink,message).await?;}},
 Ok(Event::Complete(id))if welcomed=>send_result(&sink,&dir,&id,&mut delivered).await?,
 Ok(Event::Info(next))=>{*local.info.write().unwrap()=next.clone();if !cleaned&&next["docker"]["available"]==true{let keep=active.lock().unwrap().keys().cloned().collect();if executor::cleanup_managed(&node_id,&keep).await.is_ok(){cleaned=true;}}let changed=next["executors"]!=info["executors"]||next["executionCapacity"]!=info["executionCapacity"]||next["build"]!=info["build"];info=next;if changed{send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),lifecycle.stopping||holding.load(Ordering::Acquire))).await?;}},
 Ok(Event::Update(decision))=>{match decision{UpdateDecision::Restart=>{lifecycle.restart=true;holding.store(true,Ordering::Release);},UpdateDecision::Reprobe=>{info=node_info_for(Some(&dir)).await?;*local.info.write().unwrap()=info.clone();update_only.store(false,Ordering::Release);holding.store(false,Ordering::Release);},UpdateDecision::None=>{if welcomed{send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),lifecycle.stopping||holding.load(Ordering::Acquire))).await?;}else{refresh_after_welcome=true;}}}if decision!=UpdateDecision::None{break;}},
 Ok(Event::LocalControl)=>{send_frame(&sink,local_hello(&local,&info,&active,pending_results(&dir).await?,&endpoint.config(),lifecycle.stopping||holding.load(Ordering::Acquire))).await?;},
 Ok(Event::Fatal(error))=>{lifecycle.fatal=Some(error);lifecycle.stop(&active,&abandon,false);stopping.store(true,Ordering::Release);},
 Ok(Event::Reconnect)|Err(broadcast::error::RecvError::Lagged(_))=>break,
 _=>{}
 },

 }}Ok(())}.await;
 heartbeat_task.abort();
 local.connected.store(false,Ordering::Release);
 if let Err(error)=session{eprintln!("Worker channel disconnected: {error:#}");}
 }else{failures+=1;if failures>=2{for candidate in endpoint.config()["controllerFallbackUrls"].as_array().into_iter().flatten().filter_map(Value::as_str){if endpoint.relocate(candidate).await.unwrap_or(false){failures=0;break;}}}}
 if let Some(exit)=lifecycle.exit(&dir,&active,&options).await?{break Ok(exit);}
 let wait=tokio::time::sleep(backoff);tokio::pin!(wait);loop{tokio::select!{
 _=&mut wait=>break,
 _=stop.cancelled(),if !lifecycle.stopping=>{lifecycle.stop(&active,&abandon,options.drain);stopping.store(true,Ordering::Release);},
 _=local.shutdown.cancelled(),if !lifecycle.stopping=>{lifecycle.stopping=true;lifecycle.local=true;stopping.store(true,Ordering::Release);},
 _=retry.tick()=>{if let Some(exit)=lifecycle.exit(&dir,&active,&options).await?{return Ok(exit);}},
 event=receiver.recv()=>match event{Ok(Event::Info(next))=>{*local.info.write().unwrap()=next.clone();info=next;},Ok(Event::Update(UpdateDecision::Restart))=>{lifecycle.restart=true;holding.store(true,Ordering::Release);},Ok(Event::Update(UpdateDecision::Reprobe))=>{info=node_info_for(Some(&dir)).await?;*local.info.write().unwrap()=info.clone();update_only.store(false,Ordering::Release);holding.store(false,Ordering::Release);break;},Ok(Event::Fatal(error))=>{lifecycle.fatal=Some(error);lifecycle.stop(&active,&abandon,false);stopping.store(true,Ordering::Release);},_=>{}}
 }}backoff=(backoff*2).min(options.reconnect_max);
 }}.await;
    stopping.store(true, Ordering::Release);
    runtime_stop.cancel();
    probe_task.abort();
    if let Some(updater) = updater {
        // Cancellation stops download waits inside the hook. Once a blocking installation has
        // started, keep its archive and the worker instance lock alive until it is joined.
        updater.await.context("worker update task failed")?;
    }
    result
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[tokio::test]
    async fn controller_rename_rejects_a_welcome_for_another_worker() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let original = json!({"nodeId":"worker-id","name":"DESKTOP-JEL","credential":"agent-secret","caPem":"pinned-ca","clusterId":"cluster-id"});
        state::write_json(&dir.path().join("agent.json"), &original)?;
        // The local rename path never uses this HTTP client.
        let endpoint = Endpoint {
            dir: dir.path().into(),
            config: Mutex::new(original.clone()),
            client: RwLock::new(Client {
                http: reqwest::Client::new(),
                config: Config {
                    version: 1,
                    url: "https://127.0.0.1:7300".into(),
                    ca_pem: String::new(),
                    ca_fingerprint: String::new(),
                    token: String::new(),
                    token_name: None,
                },
            }),
            relocating: tokio::sync::Mutex::new(()),
        };
        assert!(
            endpoint
                .adopt_name(&json!({"type":"welcome","nodeId":"another-worker","name":"big-box"}))
                .await
                .is_err()
        );
        assert_eq!(endpoint.config(), original);
        assert_eq!(state::read_json(&dir.path().join("agent.json"))?, original);
        endpoint
            .adopt_name(&json!({"type":"welcome","nodeId":"worker-id","name":"big-box"}))
            .await?;
        assert_eq!(endpoint.config()["name"], "big-box");
        assert_eq!(
            state::read_json(&dir.path().join("agent.json"))?["name"],
            "big-box"
        );
        Ok(())
    }

    #[test]
    fn controller_rename_preserves_saved_identity_and_local_policy() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("agent.json");
        let original = json!({"nodeId":"worker-id","credential":"agent-secret","caPem":"pinned-ca","clusterId":"cluster-id","name":"DESKTOP-JEL","settings":{"maxConcurrency":4},"settingsPending":true,"paused":true});
        state::write_json(&path, &original)?;
        let renamed = persist_controller_name(&path, &original, "big-box")?;
        let mut expected = original.clone();
        expected["name"] = json!("big-box");
        assert_eq!(renamed, expected);
        assert_eq!(state::read_json(&path)?, expected);
        let mut replaced = expected.clone();
        replaced["credential"] = json!("replacement-secret");
        state::write_json(&path, &replaced)?;
        assert!(persist_controller_name(&path, &original, "other-worker").is_err());
        assert_eq!(state::read_json(&path)?, replaced);
        assert!(persist_controller_name(&path, &replaced, "bad name").is_err());
        assert_eq!(state::read_json(&path)?, replaced);
        Ok(())
    }

    #[test]
    fn host_available_memory_counts_reclaimable_pages_without_compressor_subtraction() {
        assert_eq!(available_pages_bytes(10, 200, 16384, 1 << 30), 210 * 16384);
        assert_eq!(available_pages_bytes(u64::MAX, 1, 16384, 1 << 30), 1 << 30);
        assert_eq!(available_pages_bytes(10, 20, 0, 1 << 30), 0);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn host_available_memory_is_sampled_for_heartbeats() -> Result<()> {
        let system = sysinfo::System::new_all();
        let physical = system.total_memory();
        let sample = darwin_available_memory_bytes(physical)
            .context("Mach host memory sample must succeed on Darwin")?;
        assert!(sample > 0 && sample <= physical);
        let dir = tempfile::tempdir()?;
        let metrics = sample_metrics(dir.path(), Arc::new(Mutex::new(system))).await?;
        let heartbeat = metrics["memoryAvailableMb"].as_u64().unwrap() * 1024 * 1024;
        assert!(heartbeat <= physical);
        // Sampling is live, so allow ordinary allocation/reclamation between calls.
        assert!(heartbeat.abs_diff(sample) < physical / 4);
        Ok(())
    }

    #[test]
    fn refresh_tracks_the_sent_hello_when_update_starts_during_outbox_scan() {
        let holding = AtomicBool::new(false);
        let earlier_sample = holding.load(Ordering::Acquire);
        // This interleaving used to occur across the asynchronous initial outbox scan.
        holding.store(true, Ordering::Release);
        let active = Arc::new(Mutex::new(HashMap::new()));
        let message = hello(
            &json!({"executors":["command"]}),
            &active,
            vec![],
            &json!({}),
            holding.load(Ordering::Acquire),
        );
        holding.store(false, Ordering::Release); // hook completes before welcome is processed
        assert!(!earlier_sample);
        assert_eq!(message["info"]["executors"], json!([]));
        assert!(initial_admission_refresh(&message, false));
        assert!(
            !initial_admission_refresh(&message, true),
            "shutdown never restores admission"
        );
    }
}
