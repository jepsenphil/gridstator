//! Owner-only worker bridge and discovery. No local operation requires a cluster client token.
use crate::{
    services::{ServiceAction, ServiceConfig, ServiceManager, ServiceRole},
    state,
    worker::policy,
};
use anyhow::{Context, Result, bail, ensure};
use gridstator_local_control_protocol::{Job, LogPage, VERSION, WorkerResponse, WorkerSnapshot};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const MAX_MESSAGE: usize = 1024 * 1024;

pub(crate) fn owned_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file(),
        "FORBIDDEN: local worker file is not regular"
    );
    ensure!(
        metadata.len() <= MAX_MESSAGE as u64,
        "CONFIG: local worker file is too large"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "FORBIDDEN: this account does not own local worker data"
        );
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "FORBIDDEN: local worker state must be private to this account"
        );
    }
    Ok(())
}

pub(crate) fn owned_directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "CONFIG: worker data directory must be absolute"
    );
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_dir(),
        "FORBIDDEN: local worker directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "FORBIDDEN: this account does not own the worker directory"
        );
    }
    Ok(fs::canonicalize(path)?)
}

pub(crate) fn configuration(dir: &Path) -> Result<Value> {
    let dir = owned_directory(dir)?;
    let path = dir.join("agent.json");
    owned_regular_file(&path)?;
    let config = state::read_json(&path)?;
    ensure!(
        config["version"] == 2 && config["nodeId"].is_string() && config["credential"].is_string(),
        "CONFIG: invalid native worker registration"
    );
    Ok(config)
}

fn candidates(request: &Value) -> Result<Vec<PathBuf>> {
    if let Some(path) = request.get("dataDir") {
        let path = PathBuf::from(path.as_str().context("dataDir must be a path")?);
        return Ok(vec![owned_directory(&path)?]);
    }
    let mut paths = vec![
        state::data_dir("agent"),
        state::data_dir("controller").join("embedded-agent"),
    ];
    let registry = registry_path();
    if registry.exists() {
        owned_regular_file(&registry)?;
        let registered = state::read_json(&registry)?;
        if let Some(items) = registered["directories"].as_array() {
            ensure!(items.len() <= 64, "too many registered local workers");
            paths.extend(items.iter().filter_map(Value::as_str).map(PathBuf::from));
        }
    }
    let mut result = Vec::new();
    for path in paths {
        if path.join("agent.json").exists() {
            let dir = owned_directory(&path)?;
            if !result.contains(&dir) {
                result.push(dir);
            }
        }
    }
    Ok(result)
}

fn registry_path() -> PathBuf {
    state::data_dir("agent")
        .parent()
        .unwrap()
        .join("local-workers.json")
}

pub(crate) fn register(dir: &Path) -> Result<()> {
    let dir = owned_directory(dir)?;
    let path = registry_path();
    if path.exists() {
        owned_regular_file(&path)?;
    }
    state::update_json(&path, |saved| {
        let mut entries = saved["directories"].as_array().cloned().unwrap_or_default();
        let value = json!(dir);
        if !entries.contains(&value) {
            ensure!(entries.len() < 64, "too many local workers");
            entries.push(value);
        }
        *saved = json!({"version":VERSION,"directories":entries});
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn socket_path(dir: &Path) -> PathBuf {
    runtime_socket(dir, "worker")
}
pub(crate) fn embedded_socket_path(dir: &Path) -> PathBuf {
    runtime_socket(dir, "embedded")
}
fn runtime_socket(dir: &Path, kind: &str) -> PathBuf {
    use sha2::Digest;
    let digest = hex::encode(sha2::Sha256::digest(dir.as_os_str().as_encoded_bytes()));
    // macOS TMPDIR is often too long for sockaddr_un. A short owner-private directory keeps
    // socket paths valid even when worker data lives under a long Application Support path.
    PathBuf::from("/tmp")
        .join(format!("gridstator-control-{}", unsafe { libc::geteuid() }))
        .join(format!("{kind}-{}.sock", &digest[..24]))
}
pub(crate) fn prepare_socket_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("control socket requires a parent")?;
    match fs::create_dir(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    owned_directory(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn embedded(dir: &Path) -> bool {
    let verified = || -> Result<()> {
        let dir = owned_directory(dir)?;
        ensure!(
            dir.file_name().is_some_and(|name| name == "embedded-agent"),
            "not an embedded worker directory"
        );
        let parent = dir.parent().context("embedded worker has no controller")?;
        owned_directory(parent)?;
        let config_path = parent.join("controller.json");
        owned_regular_file(&config_path)?;
        let controller = state::read_json(&config_path)?;
        ensure!(
            controller["version"] == 2 && controller["clusterId"].is_string(),
            "invalid parent controller"
        );
        let worker = configuration(&dir)?;
        if worker["clusterId"].is_string() {
            ensure!(
                worker["clusterId"] == controller["clusterId"],
                "different cluster"
            );
        }
        // Older registrations do not store clusterId. The owned parent's CA remains the durable
        // relationship proof, rather than a directory basename or an unrelated controller PID.
        let ca_path = parent.join("pki/ca.pem");
        owned_regular_file(&ca_path)?;
        let expected = crate::pki::normalize_fingerprint(
            worker["caFingerprint"]
                .as_str()
                .context("worker CA missing")?,
        )?;
        ensure!(
            crate::pki::fingerprint(&fs::read_to_string(ca_path)?)? == expected,
            "different controller CA"
        );
        Ok(())
    };
    verified().is_ok()
}

pub(crate) fn controller_running(dir: &Path) -> bool {
    if !embedded(dir) {
        return false;
    }
    dir.parent()
        .and_then(|parent| state::read_json(&parent.join("runtime.json")).ok())
        .and_then(|runtime| runtime["pid"].as_u64())
        .is_some_and(process_alive)
}

pub(crate) fn mark_stopped(dir: &Path) -> Result<()> {
    state::write_json(
        &dir.join("local-stop.json"),
        &json!({"version":VERSION,"session":session_id(),"stopped":true}),
    )
}
pub(crate) fn clear_stopped(dir: &Path) -> Result<()> {
    let path = dir.join("local-stop.json");
    if path.exists() {
        owned_regular_file(&path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}
pub(crate) fn manually_stopped(dir: &Path) -> Result<bool> {
    let path = dir.join("local-stop.json");
    if !path.exists() {
        return Ok(false);
    }
    owned_regular_file(&path)?;
    let value = state::read_json(&path)?;
    Ok(value["version"] == VERSION && value["session"] == session_id() && value["stopped"] == true)
}
fn session_id() -> String {
    format!(
        "{}:{}",
        unsafe { libc::geteuid() },
        sysinfo::System::boot_time()
    )
}

fn process_alive(pid: u64) -> bool {
    if pid == 0 || pid > i32::MAX as u64 {
        return false;
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

pub(crate) fn snapshot(dir: &Path, config: &Value) -> Result<WorkerSnapshot> {
    let local = policy::load(&dir.join("local-policy.json"))?;
    Ok(WorkerSnapshot {
        data_dir: dir.to_string_lossy().into_owned(),
        node_id: config["nodeId"].as_str().unwrap_or("").into(),
        cluster_id: config["clusterId"].as_str().map(str::to_owned),
        ca_fingerprint: config["caFingerprint"].as_str().unwrap_or("").into(),
        name: config["name"].as_str().unwrap_or("Worker").into(),
        controller_url: config["controllerUrl"].as_str().unwrap_or("").into(),
        embedded: embedded(dir),
        controller_running: controller_running(dir),
        running: false,
        connected: false,
        state: "stopped".into(),
        paused: local.paused,
        draining: false,
        start_at_sign_in: local.start_at_sign_in,
        budget: local.budget,
        effective_budget: None,
        generation: local.generation,
        active_jobs: vec![],
        recent_jobs: recent_jobs(dir)?,
        info: None,
        controller_settings: None,
        error: None,
    })
}

pub(crate) fn recent_jobs(dir: &Path) -> Result<Vec<Job>> {
    let jobs = dir.join("jobs");
    if !jobs.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for entry in fs::read_dir(jobs)?
        .take(1024)
        .filter_map(std::result::Result::ok)
    {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let Ok(value) = state::read_json(&path.join("result.json")) else {
            continue;
        };
        let Ok(assignment) = state::read_json(&path.join("assignment.json")) else {
            continue;
        };
        let Some(id) = value["jobId"].as_str().filter(|id| valid_job_id(id)) else {
            continue;
        };
        result.push(Job {
            id: id.into(),
            name: assignment["spec"]["name"].as_str().unwrap_or(id).into(),
            phase: value["outcome"].as_str().unwrap_or("finished").into(),
            started_at: value["finishedAt"].as_i64(),
        });
    }
    let history = dir.join("recent-jobs");
    if history.exists() {
        for entry in fs::read_dir(history)?
            .take(100)
            .filter_map(std::result::Result::ok)
        {
            if entry.file_type()?.is_dir()
                && let Ok(value) = state::read_json(&entry.path().join("job.json"))
                && let Ok(job) = serde_json::from_value::<Job>(value)
            {
                result.push(job);
            }
        }
    }
    result.sort_by_key(|job| std::cmp::Reverse(job.started_at));
    result.truncate(100);
    Ok(result)
}

pub(crate) fn archive_job(dir: &Path, id: &str) -> Result<()> {
    ensure!(valid_job_id(id), "invalid local job ID");
    let current = dir.join("jobs").join(id);
    let result = state::read_json(&current.join("result.json"))?;
    let assignment = state::read_json(&current.join("assignment.json"))?;
    let history = dir.join("recent-jobs");
    state::private_dir(&history)?;
    let target = history.join(id);
    state::private_dir(&target)?;
    let job = Job {
        id: id.into(),
        name: assignment["spec"]["name"].as_str().unwrap_or(id).into(),
        phase: result["outcome"].as_str().unwrap_or("finished").into(),
        started_at: result["finishedAt"].as_i64(),
    };
    state::write_json(&target.join("job.json"), &serde_json::to_value(job)?)?;
    // Keep only a bounded masked log tail. Assignment data (including secretEnv), snapshots and
    // artifacts are not copied into local history.
    if let Ok(mut file) = fs::File::open(current.join("logs.ndjson")) {
        let begin = file.metadata()?.len().saturating_sub(1024 * 1024);
        file.seek(SeekFrom::Start(begin))?;
        let mut contents = Vec::new();
        file.take(1024 * 1024).read_to_end(&mut contents)?;
        let offset = if begin == 0 {
            0
        } else {
            contents
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(contents.len(), |position| position + 1)
        };
        state::write_atomic(&target.join("logs.ndjson"), &contents[offset..])?;
    }
    let mut entries = fs::read_dir(&history)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for old in entries.into_iter().skip(100) {
        owned_directory(&old.path())?;
        fs::remove_dir_all(old.path())?;
    }
    Ok(())
}

fn valid_job_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub(crate) fn logs(dir: &Path, request: &Value) -> Result<LogPage> {
    let id = request["jobId"]
        .as_str()
        .filter(|id| valid_job_id(id))
        .context("invalid local job ID")?;
    let running = dir.join("jobs").join(id);
    let job = if running.exists() {
        running
    } else {
        dir.join("recent-jobs").join(id)
    };
    owned_directory(&job)?;
    let path = job.join("logs.ndjson");
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.file_type().is_file(),
        "FORBIDDEN: local log must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "FORBIDDEN: local log owner mismatch"
        );
    }
    let after = request["afterSeq"].as_u64().unwrap_or(0);
    let maximum = request["limit"].as_u64().unwrap_or(300).clamp(1, 1000) as usize;
    let mut entries = vec![];
    let mut bytes = 0;
    let mut response_bytes = 0;
    let mut next = after;
    for line in BufReader::new(fs::File::open(path)?).split(b'\n') {
        let line = line?;
        bytes += line.len();
        ensure!(
            bytes <= 128 * 1024 * 1024,
            "local log scan exceeds its limit"
        );
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(sequence) = value["s"].as_u64().filter(|sequence| *sequence > after) else {
            continue;
        };
        // These are the masked durable spool fields; never read assignment secretEnv here.
        let entry = json!({"s":sequence,"t":value["t"].as_i64(),"o":crate::protocol::clip_text(value["o"].as_str().unwrap_or("system"),32),"d":crate::protocol::clip_text(value["d"].as_str().unwrap_or(""),64*1024)});
        let size = serde_json::to_vec(&entry)?.len();
        if response_bytes + size > 512 * 1024 {
            break;
        }
        response_bytes += size;
        entries.push(entry);
        next = sequence;
        if entries.len() >= maximum {
            break;
        }
    }
    Ok(LogPage {
        job_id: id.into(),
        entries,
        next_seq: next,
    })
}

#[cfg(unix)]
pub(crate) async fn send_socket(path: &Path, request: &Value) -> Result<Value> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() },
        "FORBIDDEN: worker control socket owner/type mismatch"
    );
    let stream = tokio::net::UnixStream::connect(path).await?;
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "FORBIDDEN: worker control peer owner mismatch"
    );
    let (mut reader, mut writer) = stream.into_split();
    let mut data = serde_json::to_vec(request)?;
    ensure!(data.len() < MAX_MESSAGE, "worker request too large");
    data.push(b'\n');
    writer.write_all(&data).await?;
    writer.shutdown().await?;
    let mut response = vec![];
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::BufReader::new(&mut reader)
            .take((MAX_MESSAGE + 1) as u64)
            .read_until(b'\n', &mut response),
    )
    .await
    .context("local worker control timed out")??;
    ensure!(response.len() <= MAX_MESSAGE, "worker response too large");
    let response: Value = serde_json::from_slice(&response)?;
    ensure!(
        response["ok"] == true,
        "{}",
        response["error"]
            .as_str()
            .unwrap_or("worker control failed")
    );
    Ok(response["value"].clone())
}

pub(crate) fn validate_identity(config: &Value, request: &Value) -> Result<()> {
    if let Some(expected) = request.get("nodeId") {
        ensure!(
            expected == &config["nodeId"],
            "CONFLICT: local worker identity changed"
        );
    }
    if let Some(expected) = request.get("clusterId").filter(|value| !value.is_null()) {
        ensure!(
            expected == &config["clusterId"],
            "CONFLICT: local worker cluster changed"
        );
    }
    if let Some(expected) = request.get("caFingerprint") {
        ensure!(
            expected == &config["caFingerprint"],
            "CONFLICT: local worker CA changed"
        );
    }
    Ok(())
}

pub async fn execute(request: Value) -> Result<Value> {
    let action = request["action"].as_str().unwrap_or("list");
    if action == "join" {
        return Box::pin(crate::cli::join_local_worker(&request)).await;
    }
    ensure!(
        [
            "list",
            "status",
            "pause",
            "resume",
            "setBudget",
            "start",
            "stop",
            "forceStop",
            "cancel",
            "logs",
            "setStartup"
        ]
        .contains(&action),
        "unknown worker control action"
    );
    if action == "forceStop" {
        ensure!(
            request["confirmed"] == true,
            "Force stop requires explicit confirmation"
        );
    }
    let targets = candidates(&request)?;
    if !["list", "status"].contains(&action) {
        ensure!(targets.len() == 1, "Select exactly one local worker");
    }
    let mut snapshots = vec![];
    let mut log_page = None;
    for dir in targets {
        let config = configuration(&dir)?;
        validate_identity(&config, &request)?;
        let socket = socket_path(&dir);
        let mut live_value = None;
        let mut offline_guard = None;
        let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if socket.exists() {
                match send_socket(&socket, &request).await {
                    Ok(value) => {
                        live_value = Some(value);
                        break;
                    }
                    Err(error) if control_unavailable(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            // Holding the same instance lock used by worker startup closes BOTH race windows:
            // a worker cannot load policy before this disk mutation, and a live/starting worker
            // forces this request to wait for its endpoint instead of acknowledging stale state.
            if let Some(guard) = claim_offline_instance(&dir)? {
                offline_guard = Some(guard);
                break;
            }
            ensure!(
                tokio::time::Instant::now() < ready_deadline,
                "UNAVAILABLE: local worker is starting or updating; retry control shortly"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if let Some(value) = live_value {
            if action == "setStartup" && !embedded(&dir) {
                set_service_startup(
                    &dir,
                    request["startAtSignIn"]
                        .as_bool()
                        .context("startAtSignIn required")?,
                )
                .await?;
            }
            if let Some(page) = value.get("logs") {
                log_page = Some(serde_json::from_value(page.clone())?);
            }
            snapshots.push(serde_json::from_value(value["snapshot"].clone())?);
            continue;
        }
        match action {
            "list" | "status" => {}
            "pause" | "resume" | "setBudget" | "setStartup" => {
                policy::update_file(&dir.join("local-policy.json"), action, &request)?;
                if action == "setStartup" && !embedded(&dir) {
                    set_service_startup(&dir, request["startAtSignIn"].as_bool().unwrap()).await?;
                }
            }
            "logs" => log_page = Some(logs(&dir, &request)?),
            "start" => {
                drop(offline_guard.take());
                start(&dir).await?;
            }
            "stop" | "forceStop" => {}
            "cancel" => bail!("NOT_FOUND: job is not running on this worker"),
            _ => unreachable!(),
        }
        // A service may take a moment to create its control endpoint after Start.
        if action == "start" {
            for _ in 0..40 {
                if socket.exists()
                    && let Ok(value) = send_socket(&socket, &json!({"action":"status"})).await
                {
                    snapshots.push(serde_json::from_value(value["snapshot"].clone())?);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if snapshots
                .last()
                .is_some_and(|snapshot: &WorkerSnapshot| snapshot.data_dir == dir.to_string_lossy())
            {
                continue;
            }
        }
        snapshots.push(snapshot(&dir, &config)?);
    }
    Ok(serde_json::to_value(WorkerResponse {
        version: VERSION,
        workers: snapshots,
        logs: log_page,
    })?)
}

fn claim_offline_instance(dir: &Path) -> Result<Option<fs::File>> {
    let path = dir.join("agent.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path)?;
    owned_regular_file(&path)?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn control_unavailable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|error| error.downcast_ref::<std::io::Error>())
        .any(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            )
        })
}

fn agent_service_config(dir: &Path) -> Result<ServiceConfig> {
    let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("install"));
    Ok(ServiceConfig::new(
        ServiceRole::Agent,
        root.join("supervisor"),
        root,
        dir.to_owned(),
        dirs::home_dir().context("local account home unavailable")?,
        unsafe { libc::geteuid() },
    ))
}

pub(crate) fn validate_service_definition(config: &ServiceConfig) -> Result<()> {
    let path = config.definition_path(crate::services::ServicePlatform::current()?)?;
    owned_regular_file(&path).or_else(|error| {
        // Service definitions conventionally use 0644, unlike credential/policy files.
        let metadata = fs::symlink_metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.file_type().is_file() && metadata.uid() == unsafe { libc::geteuid() },
                "FORBIDDEN: foreign worker service"
            );
        }
        if !metadata.file_type().is_file() {
            return Err(error);
        }
        Ok(())
    })?;
    let definition = fs::read_to_string(&path)?;
    #[cfg(target_os = "linux")]
    let arguments = {
        let line = definition
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .context("CONFIG: worker service has no ExecStart")?;
        crate::cli::service_words(line).context("CONFIG: invalid worker service arguments")?
    };
    #[cfg(target_os = "macos")]
    let arguments = {
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "ProgramArguments", "json", "-o", "-"])
            .arg(&path)
            .output()?;
        ensure!(
            output.status.success(),
            "CONFIG: invalid worker launch agent"
        );
        serde_json::from_slice::<Vec<String>>(&output.stdout)?
    };
    let expected = config.program_arguments()?;
    ensure!(
        arguments.len() >= expected.len() && arguments[..expected.len()] == expected,
        "FORBIDDEN: service belongs to another worker"
    );
    // No later argument may override the fenced role/data directory or inject another program.
    ensure!(
        !arguments[expected.len()..]
            .iter()
            .any(|arg| arg == "--data-dir" || arg.starts_with("--data-dir=") || arg == "--"),
        "FORBIDDEN: worker service overrides its data directory"
    );
    Ok(())
}

async fn start(dir: &Path) -> Result<()> {
    clear_stopped(dir)?;
    if embedded(dir) {
        let socket = embedded_socket_path(dir.parent().unwrap());
        send_socket(&socket, &json!({"action":"start"})).await?;
        return Ok(());
    }
    let config = agent_service_config(dir)?;
    validate_service_definition(&config)?;
    tokio::task::spawn_blocking(move || {
        ServiceManager::current()?.control(&config, ServiceAction::Start)
    })
    .await??;
    Ok(())
}

pub(crate) async fn set_service_startup(dir: &Path, enabled: bool) -> Result<()> {
    let config = agent_service_config(dir)?;
    let path = config.definition_path(crate::services::ServicePlatform::current()?)?;
    if !path.exists() {
        return Ok(());
    }
    validate_service_definition(&config)?;
    let action = if enabled { "enable" } else { "disable" };
    #[cfg(target_os = "linux")]
    {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("systemctl")
                .args(["--user", action, &config.unit_name()])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("worker startup service command timed out")??;
        ensure!(
            output.status.success(),
            "Could not update worker startup preference"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{}", config.uid, config.label);
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("launchctl")
                .args([action, &target])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("worker startup service command timed out")??;
        ensure!(
            output.status.success(),
            "Could not update worker startup preference"
        );
    }
    Ok(())
}

pub async fn host(request: &Value) -> Result<Value> {
    let mut idle = std::time::Instant::now();
    loop {
        let targets = candidates(request)?;
        let alive = targets.iter().any(|dir| {
            std::os::unix::net::UnixStream::connect(socket_path(dir)).is_ok()
                || controller_running(dir)
        });
        // A controller can be running before its embedded registration is written.
        let controller = state::read_json(&state::data_dir("controller").join("runtime.json"))
            .ok()
            .and_then(|value| value["pid"].as_u64())
            .is_some_and(process_alive);
        if alive || controller {
            idle = std::time::Instant::now();
        }
        if idle.elapsed() > Duration::from_secs(30) {
            return Ok(json!({"version":VERSION,"state":"idle"}));
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=state::shutdown_signal()=>return Ok(json!({"version":VERSION,"state":"closed"}))}
    }
}

pub(crate) fn service_config_for_install(dir: &Path) -> Result<ServiceConfig> {
    agent_service_config(dir)
}
