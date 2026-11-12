use crate::{
    db::{self, Db},
    protocol, state,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, watch};

/// Production defaults match the retained controller. Tests shorten these intervals instead of
/// sleeping for a minute or changing process-wide clocks.
#[derive(Clone, Debug)]
pub struct ControllerTiming {
    pub heartbeat_timeout_ms: i64,
    pub lost_timeout_ms: i64,
    pub cancel_timeout_ms: i64,
    pub assign_timeout_ms: i64,
    pub startup_grace_ms: i64,
    pub rejection_ttl_ms: i64,
    pub sweep_interval: Duration,
    pub schedule_interval: Duration,
    pub retention_interval: Duration,
}
impl Default for ControllerTiming {
    fn default() -> Self {
        Self {
            heartbeat_timeout_ms: 20_000,
            lost_timeout_ms: 60_000,
            cancel_timeout_ms: 30_000,
            assign_timeout_ms: 30_000,
            startup_grace_ms: 60_000,
            rejection_ttl_ms: 600_000,
            sweep_interval: Duration::from_secs(2),
            schedule_interval: Duration::from_secs(5),
            retention_interval: Duration::from_secs(3600),
        }
    }
}

#[derive(Clone, Debug)]
struct CloseNotice {
    code: u16,
    reason: String,
}

#[derive(Clone)]
struct AgentConnection {
    sender: mpsc::Sender<Value>,
    generation: String,
    token_id: String,
    last_message_at: i64,
    close: watch::Sender<Option<CloseNotice>>,
}
pub struct Controller {
    pub db: Db,
    pub dir: PathBuf,
    pub config: Value,
    pub releases: crate::release::OfficialReleases,
    events: broadcast::Sender<Value>,
    agents: Mutex<HashMap<String, AgentConnection>>,
    io_permits: Arc<tokio::sync::Semaphore>,
    restart: tokio::sync::Notify,
    started_at: i64,
    timing: ControllerTiming,
    lifecycle: tokio::sync::Mutex<()>,
    disconnected: Mutex<HashMap<String, i64>>,
    client_sessions: Mutex<HashMap<String, (String, watch::Sender<bool>)>>,
    shutdown: watch::Sender<bool>,
    cas_lifecycle: Arc<tokio::sync::RwLock<()>>,
    _instance: Arc<std::fs::File>,
}
type Shared = Arc<Controller>;
pub struct ApiError(anyhow::Error);
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}
// Keep the established untagged request-validation errors while preserving domain codes and
// preventing infrastructure failures from masquerading as invalid input.
fn api_error_status(error: &anyhow::Error) -> (&'static str, StatusCode, bool) {
    const CODES: [(&str, u16); 10] = [
        ("BAD_REQUEST", 400),
        ("UNAUTHORIZED", 401),
        ("FORBIDDEN", 403),
        ("NOT_FOUND", 404),
        ("CONFLICT", 409),
        ("PAYLOAD_TOO_LARGE", 413),
        ("UNAVAILABLE", 503),
        ("TLS_PIN_MISMATCH", 495),
        ("CONFIG", 500),
        ("INTERNAL", 500),
    ];
    for cause in error.chain() {
        let message = cause.to_string();
        if let Some((code, status)) = CODES.iter().find(|(code, _)| {
            message
                .strip_prefix(code)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'))
        }) {
            return (
                code,
                StatusCode::from_u16(*status).expect("known API error status"),
                false,
            );
        }
    }
    if error.chain().any(|cause| {
        cause.is::<std::io::Error>()
            || cause.is::<rusqlite::Error>()
            || cause.is::<tokio::task::JoinError>()
    }) {
        ("INTERNAL", StatusCode::INTERNAL_SERVER_ERROR, true)
    } else {
        ("BAD_REQUEST", StatusCode::BAD_REQUEST, false)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, status, unexpected) = api_error_status(&self.0);
        let message = if unexpected {
            eprintln!("unhandled controller API error: {:#}", self.0);
            "Internal controller error (see controller logs)".to_owned()
        } else {
            self.0.to_string()
        };
        (
            status,
            Json(json!({"error":{"code":code,"message":message}})),
        )
            .into_response()
    }
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
fn mint(kind: &str) -> String {
    format!(
        "{}_{}",
        match kind {
            "join" => "gsj",
            "agent" => "gsa",
            _ => "gsc",
        },
        hex::encode(rand::random::<[u8; 32]>())
    )
}
pub fn issue_token(
    c: &rusqlite::Connection,
    kind: &str,
    name: &str,
    role: &str,
    node: Option<&str>,
    uses: Option<u64>,
    expires: Option<i64>,
) -> Result<Value> {
    let raw = mint(kind);
    let id = state::id();
    let record = json!({"id":id,"kind":kind,"name":name,"role":if kind=="client"{json!(role)}else{Value::Null},"nodeId":node,"hash":token_hash(&raw),"usesRemaining":uses,"expiresAt":expires,"revokedAt":null,"createdAt":state::now(),"lastUsedAt":null});
    db::put(c, "tokens", &id, &record)?;
    let mut view = record;
    view.as_object_mut().unwrap().remove("hash");
    view["token"] = json!(raw);
    Ok(view)
}
fn authenticate(c: &rusqlite::Connection, raw: &str, kinds: &[&str], admin: bool) -> Result<Value> {
    let hash = token_hash(raw);
    let mut p = db::list(c, "tokens")?
        .into_iter()
        .find(|t| t["hash"] == hash)
        .context("UNAUTHORIZED: invalid token")?;
    ensure!(
        p["revokedAt"].is_null()
            && p["expiresAt"].as_i64().is_none_or(|e| e > state::now())
            && p["usesRemaining"].as_u64() != Some(0),
        "UNAUTHORIZED: expired or revoked token"
    );
    ensure!(
        kinds.contains(&p["kind"].as_str().unwrap_or("")),
        "FORBIDDEN: token kind cannot perform this action"
    );
    ensure!(
        !admin || p["role"] == "admin",
        "FORBIDDEN: administrator required"
    );
    p["lastUsedAt"] = json!(state::now());
    db::put(c, "tokens", p["id"].as_str().unwrap(), &p)?;
    Ok(p)
}
fn listener_url(host: &str, port: u64) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("https://[{host}]:{port}")
    } else {
        format!("https://{host}:{port}")
    }
}
fn decode_path_component(encoded: &str) -> Result<String> {
    let raw = encoded.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let hex = raw
                .get(index + 1..index + 3)
                .context("invalid percent encoding")?;
            let hex = std::str::from_utf8(hex)?;
            bytes.push(u8::from_str_radix(hex, 16).context("invalid percent encoding")?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes).context("path is not valid UTF-8")
}
pub async fn initialize(
    dir: &Path,
    host: &str,
    port: u16,
    extra: &[String],
    force: bool,
    config_path: &Path,
) -> Result<Value> {
    if dir.join("controller.json").exists() {
        ensure!(
            force,
            "controller already initialized; use --force to archive it"
        );
        let backup = dir.with_extension(format!("bak-{}", state::id()));
        std::fs::rename(dir, &backup)?;
    }
    state::private_dir(dir)?;
    let cluster = state::id();
    let mut sans = extra.to_vec();
    sans.push(host.into());
    let fingerprint = crate::pki::generate(dir, &cluster, &sans)?;
    let config =
        json!({"version":2,"clusterId":cluster,"host":host,"port":port,"createdAt":state::now()});
    state::write_json(&dir.join("controller.json"), &config)?;
    let database = Db::open(&dir.join("controller.db"))?;
    let token = database
        .write(|c| issue_token(c, "client", "local-admin", "admin", None, None, None))
        .await?;
    let url = listener_url(host, port.into());
    let config = crate::client::Config {
        version: 1,
        url: url.clone(),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: fingerprint.clone(),
        token: token["token"].as_str().unwrap().into(),
        token_name: Some("local-admin".into()),
    };
    state::write_json(config_path, &serde_json::to_value(config)?)?;
    Ok(
        json!({"clusterId":cluster,"url":url,"caFingerprint":fingerprint,"token":token["token"],"configPath":config_path}),
    )
}

fn safe_job(mut job: Value) -> Value {
    job["spec"] = protocol::redact_spec(&job["spec"]);
    if let Some(object) = job.as_object_mut() {
        object.remove("retriesLeft");
    }
    job
}
fn dispatched(job: &Value) -> bool {
    protocol::reserves_resources(job["state"].as_str().unwrap_or(""))
}
fn display_name(spec: &Value) -> String {
    if let Some(name) = spec["name"].as_str() {
        return name.to_owned();
    }
    let shown = match spec["kind"].as_str() {
        Some("command") => {
            let args = spec["command"].as_array().cloned().unwrap_or_default();
            if args.len() >= 3 && args[0] == "sh" && args[1] == "-c" {
                args[2].as_str().unwrap_or("").to_owned()
            } else {
                args.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
        Some("docker-build") => {
            let context = spec["context"].as_str().unwrap_or(".");
            let file = spec["dockerfile"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if context == "." {
                        "Dockerfile".into()
                    } else {
                        format!("{context}/Dockerfile")
                    }
                });
            format!(
                "build {file}{}",
                spec["stage"]
                    .as_str()
                    .map(|stage| format!(" --target {stage}"))
                    .unwrap_or_default()
            )
        }
        Some("actions") => format!(
            "actions {}{} on {}",
            spec["workflow"]
                .as_str()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or(""),
            spec["job"]
                .as_str()
                .map(|job| format!(" ({job})"))
                .unwrap_or_default(),
            spec["event"].as_str().unwrap_or("push")
        ),
        _ => "job".into(),
    };
    if shown.encode_utf16().count() > 80 {
        format!("{}...", protocol::clip_text(&shown, 77))
    } else {
        shown
    }
}
fn default_settings() -> Value {
    json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"follow","updateWindow":null,"toolchain":{"act":null}})
}
fn cluster_settings(c: &rusqlite::Connection) -> Result<Value> {
    let mut value = default_settings();
    if let Some(saved) = db::get(c, "settings", "cluster")?
        && let Some(saved) = saved.as_object()
    {
        for (key, item) in saved {
            value[key] = item.clone();
        }
    }
    Ok(value)
}
fn patch_cluster_settings(mut settings: Value, patch: &Value) -> Result<Value> {
    for (key, value) in patch.as_object().context("settings must be an object")? {
        if [
            "jobRetentionDays",
            "logRetentionDays",
            "snapshotRetentionDays",
            "autoUpdate",
            "updateWindow",
            "toolchain",
        ]
        .contains(&key.as_str())
        {
            settings[key] = value.clone();
        }
    }
    for key in [
        "jobRetentionDays",
        "logRetentionDays",
        "snapshotRetentionDays",
    ] {
        ensure!(
            settings[key]
                .as_u64()
                .is_some_and(|days| (1..=3650).contains(&days)),
            "{key} must be 1..3650"
        );
    }
    ensure!(
        matches!(
            settings["autoUpdate"].as_str(),
            Some("off" | "follow" | "window")
        ),
        "invalid auto-update mode"
    );
    ensure!(
        settings["autoUpdate"] != "window" || !settings["updateWindow"].is_null(),
        "autoUpdate window needs an update window"
    );
    if !settings["updateWindow"].is_null() {
        let window = &settings["updateWindow"];
        let valid = |value: &Value| {
            value.as_str().is_some_and(|text| {
                let bytes = text.as_bytes();
                bytes.len() == 5
                    && bytes[2] == b':'
                    && bytes[0..2].iter().all(u8::is_ascii_digit)
                    && bytes[3..5].iter().all(u8::is_ascii_digit)
                    && text[0..2].parse::<u8>().is_ok_and(|hour| hour < 24)
                    && text[3..5].parse::<u8>().is_ok_and(|minute| minute < 60)
            })
        };
        ensure!(
            valid(&window["start"]) && valid(&window["end"]) && window["start"] != window["end"],
            "updateWindow must contain distinct HH:MM start/end"
        );
        settings["updateWindow"] = json!({"start":window["start"],"end":window["end"]});
    }
    let tools = settings["toolchain"]
        .as_object()
        .context("toolchain must be an object")?;
    let act = tools.get("act").cloned().unwrap_or(Value::Null);
    ensure!(
        act.is_null()
            || act == "off"
            || act.as_str().is_some_and(|version| {
                let parts = version.split('.').collect::<Vec<_>>();
                parts.len() == 3
                    && parts.iter().all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
            }),
        "act must be null, off or a version like 0.2.97"
    );
    settings["toolchain"] = json!({"act":act});
    Ok(settings)
}
pub fn create_job(
    c: &rusqlite::Connection,
    spec: Value,
    owner: &str,
    group: Option<(&str, u64, u64)>,
    retry: Option<&Value>,
) -> Result<Value> {
    if let Some(snapshot) = spec["snapshot"].as_str() {
        let mut record = db::get(c, "snapshots", snapshot)?.with_context(|| {
            retry.map_or_else(
                || format!("BAD_REQUEST: Snapshot {snapshot} has not been uploaded"),
                |job| format!("CONFLICT: The snapshot for job {} has been garbage-collected; run the command again", job["id"].as_str().unwrap_or("")),
            )
        })?;
        record["lastUsedAt"] = json!(state::now());
        db::put(c, "snapshots", snapshot, &record)?;
    }
    if let Some(target) = spec["target"]["node"].as_str() {
        ensure!(
            db::get(c, "nodes", target)?.is_some()
                || db::list(c, "nodes")?
                    .iter()
                    .any(|node| node["name"] == target),
            "NOT_FOUND: No node with id or name \"{target}\""
        );
    }
    let id = state::id();
    let now = state::now();
    let name = display_name(&spec);
    let job = json!({"id":id,"name":name,"kind":spec["kind"],"state":"queued","stateReason":"waiting for scheduler","priority":spec["priority"],"owner":owner,"snapshotId":spec.get("snapshot").cloned().unwrap_or(Value::Null),"spec":spec,"nodeId":null,"nodeName":null,"assignmentId":null,"attempt":retry.map(|j|j["attempt"].as_u64().unwrap_or(1)+1).unwrap_or(1),"retryOf":retry.map(|j|j["id"].clone()),"retriesLeft":spec["retry"]["max"],"exitCode":null,"error":null,"queuePosition":null,"logSeq":0,"outputs":null,"artifacts":[],"groupId":group.map(|g|g.0),"shardIndex":group.map(|g|g.1),"shardTotal":group.map(|g|g.2),"createdAt":now,"queuedAt":now,"assignedAt":null,"startedAt":null,"finishedAt":null,"cancelRequestedAt":null});
    db::put(c, "jobs", &id, &job)?;
    record_event(
        c,
        &job,
        "state",
        owner,
        None,
        Some("created"),
        retry
            .and_then(|j| j["id"].as_str())
            .map(|prior| format!("retry of {prior}"))
            .as_deref(),
    )?;
    record_event(
        c,
        &job,
        "state",
        "controller",
        Some("created"),
        Some("queued"),
        None,
    )?;
    Ok(job)
}
fn record_event(
    c: &rusqlite::Connection,
    job: &Value,
    kind: &str,
    actor: &str,
    from: Option<&str>,
    to: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    let sequence = db::get(c, "meta", "audit-sequence")?
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
        + 1;
    db::put(c, "meta", "audit-sequence", &json!(sequence))?;
    db::put(
        c,
        "events",
        &format!("{sequence:020}"),
        &json!({"id":sequence,"jobId":job["id"],"ts":state::now(),"type":kind,"fromState":from,"toState":to,"actor":actor,"message":message}),
    )
}
fn transition_as(
    c: &rusqlite::Connection,
    job: &mut Value,
    to: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<()> {
    let from = job["state"]
        .as_str()
        .context("stored job has no state")?
        .to_owned();
    ensure!(
        protocol::can_transition(&from, to),
        "CONFLICT: illegal job transition {from} to {to}"
    );
    job["state"] = json!(to);
    job["stateReason"] = if protocol::terminal(to) {
        Value::Null
    } else {
        json!(reason)
    };
    if protocol::terminal(to) && job["finishedAt"].is_null() {
        job["finishedAt"] = json!(state::now());
    }
    if to == "running" && job["startedAt"].is_null() {
        job["startedAt"] = json!(state::now());
    }
    if to != "queued" {
        job["queuePosition"] = Value::Null;
    }
    db::put(c, "jobs", job["id"].as_str().unwrap(), job)?;
    record_event(c, job, "state", actor, Some(&from), Some(to), reason)
}
fn transition(
    c: &rusqlite::Connection,
    job: &mut Value,
    to: &str,
    reason: Option<&str>,
) -> Result<()> {
    transition_as(c, job, to, reason, "controller")
}
fn requeue(c: &rusqlite::Connection, job: &mut Value, reason: &str, actor: &str) -> Result<()> {
    for key in ["nodeId", "nodeName", "assignmentId", "assignedAt"] {
        job[key] = Value::Null;
    }
    transition_as(c, job, "queued", Some(reason), actor)
}
/// All terminal paths share retry accounting, including reconnection/lost-timeout paths.
fn finish_job(
    c: &rusqlite::Connection,
    job: &mut Value,
    outcome: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<Vec<Value>> {
    transition_as(c, job, outcome, reason, actor)?;
    db::delete(c, "rejections", job["id"].as_str().unwrap())?;
    let mut changed = vec![job.clone()];
    let remaining = job["retriesLeft"].as_u64().unwrap_or(0);
    if matches!(outcome, "lost" | "failed")
        && job["cancelRequestedAt"].is_null()
        && remaining > 0
        && job["spec"]["retry"]["on"]
            .as_array()
            .is_some_and(|items| items.contains(&json!(outcome)))
    {
        let group = job["groupId"].as_str().map(|id| {
            (
                id,
                job["shardIndex"].as_u64().unwrap_or(1),
                job["shardTotal"].as_u64().unwrap_or(1),
            )
        });
        match create_job(
            c,
            job["spec"].clone(),
            job["owner"].as_str().unwrap_or("client"),
            group,
            Some(job),
        ) {
            Ok(mut retry) => {
                retry["retriesLeft"] = json!(remaining - 1);
                db::put(c, "jobs", retry["id"].as_str().unwrap(), &retry)?;
                record_event(
                    c,
                    job,
                    "retried",
                    "controller",
                    None,
                    None,
                    Some(&format!(
                        "automatic retry ({outcome}) as {}",
                        retry["id"].as_str().unwrap()
                    )),
                )?;
                changed.push(retry);
            }
            Err(error) => record_event(
                c,
                job,
                "retry-failed",
                "controller",
                None,
                None,
                Some(&error.to_string()),
            )?,
        }
    }
    Ok(changed)
}
fn validate_node_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "BAD_REQUEST: Node names must be 1..64 characters, start with a letter or digit, and contain only letters, digits, '.', '_' or '-'"
    );
    Ok(())
}

fn rename_node_references(c: &rusqlite::Connection, old_name: &str, id: &str) -> Result<()> {
    for mut job in db::list(c, "jobs")? {
        let Some(target) = job["spec"].get_mut("target").and_then(Value::as_object_mut) else {
            continue;
        };
        let mut changed = false;
        if target.get("node").and_then(Value::as_str) == Some(old_name) {
            target.insert("node".into(), json!(id));
            changed = true;
        }
        if let Some(preferred) = target.get_mut("prefer").and_then(Value::as_array_mut) {
            for name in preferred {
                if name.as_str() == Some(old_name) {
                    *name = json!(id);
                    changed = true;
                }
            }
        }
        if changed {
            db::put(
                c,
                "jobs",
                job["id"].as_str().context("job id required")?,
                &job,
            )?;
        }
    }
    Ok(())
}

fn merge_node_settings(mut settings: Value, patch: &Value) -> Result<Value> {
    if patch.is_null() {
        return Ok(settings);
    }
    for (key, value) in patch
        .as_object()
        .context("node settings must be an object")?
    {
        if key == "reserved" {
            for (k, v) in value.as_object().context("reserved must be an object")? {
                if ["cpus", "memoryMb"].contains(&k.as_str()) {
                    settings["reserved"][k] = v.clone();
                }
            }
        } else if ["maxConcurrency", "labels", "schedulingMode", "autoUpdate"]
            .contains(&key.as_str())
        {
            settings[key] = value.clone();
        }
    }
    ensure!(
        settings["maxConcurrency"]
            .as_u64()
            .is_some_and(|n| (1..=256).contains(&n)),
        "maxConcurrency must be 1..256"
    );
    ensure!(
        settings["reserved"]["cpus"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=1024.0).contains(&n)),
        "invalid reserved CPU count"
    );
    ensure!(
        settings["reserved"]["memoryMb"].as_u64().is_some(),
        "invalid reserved memory"
    );
    ensure!(
        matches!(
            settings["schedulingMode"].as_str(),
            Some("enabled" | "disabled" | "draining")
        ),
        "invalid scheduling mode"
    );
    ensure!(
        matches!(
            settings["autoUpdate"].as_str(),
            Some("inherit" | "off" | "follow" | "window")
        ),
        "invalid auto-update mode"
    );
    let labels = settings["labels"]
        .as_array()
        .context("labels must be an array")?;
    ensure!(
        labels.len() <= 64
            && labels.iter().all(|v| v
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 128)),
        "invalid node labels"
    );
    Ok(settings)
}
fn node_view(c: &rusqlite::Connection, mut node: Value, online: bool) -> Result<Value> {
    let jobs = db::list(c, "jobs")?
        .into_iter()
        .filter(|job| job["nodeId"] == node["id"] && dispatched(job))
        .collect::<Vec<_>>();
    node["activeJobs"] = json!(jobs.iter().map(|job| job["id"].clone()).collect::<Vec<_>>());
    node["allocated"] = json!({"cpus":jobs.iter().map(|job|job["spec"]["resources"]["cpus"].as_f64().unwrap_or(0.0)).sum::<f64>(),"memoryMb":jobs.iter().map(|job|job["spec"]["resources"]["memoryMb"].as_u64().unwrap_or(0)).sum::<u64>()});
    let (cpu, memory) = crate::scheduler::allocatable(&node);
    node["allocatable"] = json!({"cpus":cpu,"memoryMb":memory as u64});
    node["status"] = json!(if online { "online" } else { "offline" });
    let mut issues = vec![];
    if online {
        if node["info"]["docker"]["available"] != true {
            issues.push(format!(
                "Docker unavailable: {}",
                node["info"]["docker"]["error"]
                    .as_str()
                    .unwrap_or("daemon unreachable")
            ));
        }
        if node["metrics"]["diskFreeMb"]
            .as_u64()
            .is_some_and(|bytes| bytes < 5 * 1024)
        {
            issues.push("Low disk space".into());
        }
    } else {
        node["metrics"] = Value::Null;
    }
    node["health"] = json!(if !online {
        "unknown"
    } else if issues.is_empty() {
        "ok"
    } else {
        "degraded"
    });
    node["healthIssues"] = json!(issues);
    let mut labels = vec![];
    if node["info"].is_object() {
        labels.push(json!(format!(
            "os={}",
            node["info"]["platform"].as_str().unwrap_or("unknown")
        )));
        labels.push(json!(format!(
            "arch={}",
            node["info"]["arch"].as_str().unwrap_or("unknown")
        )));
        for tool in ["docker", "buildx", "act"] {
            if node["info"][tool]["available"] == true {
                labels.push(json!(tool));
            }
        }
    }
    for label in node["settings"]["labels"].as_array().into_iter().flatten() {
        if !labels.contains(label) {
            labels.push(label.clone());
        }
    }
    node["effectiveLabels"] = json!(labels);
    if let Some(object) = node.as_object_mut() {
        object.remove("updateHold");
        object.remove("updateOnly");
    }
    Ok(node)
}
fn refresh_nodes(c: &rusqlite::Connection, online: &HashSet<String>) -> Result<Vec<Value>> {
    let mut views = vec![];
    for node in db::list(c, "nodes")? {
        let id = node["id"].as_str().context("node ID")?.to_owned();
        let mut view = node_view(c, node.clone(), online.contains(&id))?;
        // Native updater bookkeeping is durable but never leaks through ordinary node JSON.
        for key in ["updateHold", "updateOnly"] {
            if let Some(value) = node.get(key) {
                view[key] = value.clone();
            }
        }
        db::put(c, "nodes", &id, &view)?;
        let public = node_view(c, view, online.contains(&id))?;
        views.push(public);
    }
    Ok(views)
}
fn group_view(c: &rusqlite::Connection, id: &str) -> Result<Value> {
    let mut group = db::get(c, "groups", id)?.context("NOT_FOUND: group")?;
    let mut latest = HashMap::<u64, Value>::new();
    for job in db::list(c, "jobs")? {
        if job["groupId"] != id {
            continue;
        }
        let shard = job["shardIndex"].as_u64().unwrap_or(0);
        if latest
            .get(&shard)
            .is_none_or(|prior| prior["id"].as_str() < job["id"].as_str())
        {
            latest.insert(shard, job);
        }
    }
    let mut jobs = latest.into_values().collect::<Vec<_>>();
    jobs.sort_by_key(|job| job["shardIndex"].as_u64());
    let all_terminal = !jobs.is_empty()
        && jobs
            .iter()
            .all(|job| protocol::terminal(job["state"].as_str().unwrap()));
    let state = if all_terminal {
        if jobs
            .iter()
            .any(|job| job["state"] == "failed" || job["state"] == "lost")
        {
            "failed"
        } else if jobs.iter().any(|job| job["state"] == "cancelled") {
            "cancelled"
        } else {
            "succeeded"
        }
    } else if jobs
        .iter()
        .any(|job| job["state"] != "queued" && job["state"] != "created")
    {
        "running"
    } else {
        "queued"
    };
    let mut counts = serde_json::Map::new();
    let mut nodes = std::collections::BTreeSet::new();
    for job in &jobs {
        let state = job["state"].as_str().unwrap();
        let count = counts.get(state).and_then(Value::as_u64).unwrap_or(0);
        counts.insert(state.into(), json!(count + 1));
        if let Some(node) = job["nodeName"].as_str() {
            nodes.insert(node.to_owned());
        }
    }
    group["state"] = json!(state);
    group["counts"] = json!(counts);
    group["nodes"] = json!(nodes);
    group["startedAt"] = json!(
        jobs.iter()
            .filter_map(|job| job["startedAt"].as_i64())
            .min()
    );
    group["finishedAt"] = if all_terminal {
        json!(
            jobs.iter()
                .filter_map(|job| job["finishedAt"].as_i64())
                .max()
        )
    } else {
        Value::Null
    };
    group["jobs"] = json!(jobs.into_iter().map(safe_job).collect::<Vec<_>>());
    Ok(group)
}
fn snapshot_view(mut snapshot: Value, manifest: bool) -> Value {
    if !manifest {
        snapshot.as_object_mut().unwrap().remove("manifest");
    }
    snapshot
}
fn valid_credential(
    c: &rusqlite::Connection,
    id: &str,
    kind: &str,
    node: Option<&str>,
    now: i64,
) -> Result<bool> {
    Ok(db::get(c, "tokens", id)?.is_some_and(|token| {
        token["kind"] == kind
            && token["revokedAt"].is_null()
            && token["expiresAt"].as_i64().is_none_or(|at| at > now)
            && node.is_none_or(|node| token["nodeId"] == node)
    }))
}
fn validate_node_info(info: &Value) -> Result<()> {
    ensure!(
        info["agentVersion"]
            .as_str()
            .is_some_and(|v| v.split('.').next() == Some("2")),
        "PROTOCOL_MISMATCH: native v2 agents are required; create a fresh cluster and rejoin"
    );
    ensure!(info.is_object(), "node info must be an object");
    for (key, limit) in [
        ("hostname", 255),
        ("platform", 32),
        ("arch", 32),
        ("osRelease", 255),
        ("agentVersion", 64),
    ] {
        ensure!(
            info[key]
                .as_str()
                .is_some_and(|value| value.encode_utf16().count() <= limit),
            "invalid node info {key}"
        );
    }
    for key in ["cpus", "memoryTotalMb"] {
        ensure!(
            info[key].as_u64().is_some_and(|value| value > 0),
            "invalid node info {key}"
        );
    }
    for key in ["docker", "buildx", "act"] {
        ensure!(
            info[key]["available"].is_boolean(),
            "invalid tool status {key}"
        );
    }
    let executors = info["executors"]
        .as_array()
        .context("executors must be an array")?;
    ensure!(
        executors.len() <= 32 && executors.iter().all(Value::is_string),
        "invalid executor capabilities"
    );
    Ok(())
}
fn validate_metrics(metrics: &Value) -> Result<()> {
    ensure!(
        metrics["ts"].as_u64().is_some(),
        "metrics timestamp required"
    );
    ensure!(
        metrics["cpuPercent"]
            .as_f64()
            .is_some_and(|value| (0.0..=100.0).contains(&value)),
        "invalid CPU percentage"
    );
    for key in ["memoryAvailableMb", "diskFreeMb", "diskTotalMb"] {
        ensure!(metrics[key].as_u64().is_some(), "invalid metrics {key}");
    }
    ensure!(
        metrics["load1"].as_f64().is_some_and(|value| value >= 0.0),
        "invalid load average"
    );
    Ok(())
}

impl Controller {
    pub async fn cancel_node_jobs(self: &Arc<Self>, node_id: &str, reason: &str) -> Result<()> {
        let node = node_id.to_owned();
        let jobs = self
            .db
            .call(move |c| {
                Ok(db::list(c, "jobs")?
                    .into_iter()
                    .filter(|job| job["nodeId"] == node && dispatched(job))
                    .collect::<Vec<_>>())
            })
            .await?;
        for job in jobs {
            self.cancel_job(job["id"].as_str().unwrap(), "updater", reason)
                .await?;
        }
        Ok(())
    }
    pub fn send_agent(&self, node: &str, message: Value) -> bool {
        self.agents
            .lock()
            .unwrap()
            .get(node)
            .is_some_and(|connection| connection.sender.try_send(message).is_ok())
    }
    pub fn request_restart(&self) {
        self.restart.notify_one();
    }
    pub async fn open(dir: PathBuf) -> Result<Arc<Self>> {
        Self::open_with_timing(dir, ControllerTiming::default()).await
    }
    pub async fn open_with_timing(dir: PathBuf, timing: ControllerTiming) -> Result<Arc<Self>> {
        Self::open_with_sources(
            dir,
            timing,
            crate::release::OfficialReleases::github("jepsenphil/gridstator")?,
        )
        .await
    }
    pub async fn open_with_releases(
        dir: PathBuf,
        releases: crate::release::OfficialReleases,
    ) -> Result<Arc<Self>> {
        Self::open_with_sources(dir, ControllerTiming::default(), releases).await
    }
    async fn open_with_sources(
        dir: PathBuf,
        timing: ControllerTiming,
        releases: crate::release::OfficialReleases,
    ) -> Result<Arc<Self>> {
        ensure!(
            timing.heartbeat_timeout_ms > 0
                && timing.lost_timeout_ms > 0
                && timing.cancel_timeout_ms > 0
                && timing.assign_timeout_ms > 0
                && timing.startup_grace_ms >= 0
                && timing.rejection_ttl_ms > 0,
            "invalid controller timeouts"
        );
        ensure!(
            !timing.sweep_interval.is_zero()
                && !timing.schedule_interval.is_zero()
                && !timing.retention_interval.is_zero(),
            "invalid controller intervals"
        );
        state::private_dir(&dir)?;
        let lock_path = dir.join("controller.lock");
        let instance = Arc::new(
            tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true).create(true).truncate(false);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                let file = options.open(lock_path)?;
                fs2::FileExt::try_lock_exclusive(&file)
                    .context("another controller owns this data directory")?;
                Ok(file)
            })
            .await??,
        );
        let recovery = dir.clone();
        let recovery_lease = instance.clone();
        tokio::task::spawn_blocking(move || {
            let _instance = recovery_lease;
            crate::updates::recover_before_open(&recovery)
        })
        .await??;
        let config = state::read_json(&dir.join("controller.json"))?;
        ensure!(
            config["version"] == 2,
            "refusing non-v2 controller data directory"
        );
        let db = Db::open_with_guard(&dir.join("controller.db"), instance.clone())?;
        db.write(|c| {
            if db::get(c, "settings", "cluster")?.is_none() {
                db::put(c, "settings", "cluster", &default_settings())?;
            }
            Ok(())
        })
        .await?;
        let (events, _) = broadcast::channel(2048);
        Ok(Arc::new(Self {
            db,
            dir,
            config,
            releases,
            events,
            agents: Mutex::new(HashMap::new()),
            io_permits: Arc::new(tokio::sync::Semaphore::new(4)),
            restart: tokio::sync::Notify::new(),
            started_at: state::now(),
            timing,
            lifecycle: tokio::sync::Mutex::new(()),
            disconnected: Mutex::new(HashMap::new()),
            client_sessions: Mutex::new(HashMap::new()),
            shutdown: watch::channel(false).0,
            cas_lifecycle: Arc::new(tokio::sync::RwLock::new(())),
            _instance: instance,
        }))
    }
    async fn principal(&self, headers: &HeaderMap, kinds: &[&str], admin: bool) -> Result<Value> {
        let token = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .context("UNAUTHORIZED: bearer token required")?
            .to_owned();
        let kinds = kinds
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        self.db
            .write(move |c| {
                authenticate(
                    c,
                    &token,
                    &kinds.iter().map(String::as_str).collect::<Vec<_>>(),
                    admin,
                )
            })
            .await
    }
    fn online(&self) -> HashSet<String> {
        self.agents.lock().unwrap().keys().cloned().collect()
    }
    fn emit_job(&self, job: Value) {
        if let Some(group) = job["groupId"].as_str() {
            let id = group.to_owned();
            let db = self.db.clone();
            let events = self.events.clone();
            tokio::spawn(async move {
                if let Ok(group) = db.call(move |c| group_view(c, &id)).await {
                    let _ = events.send(json!({"type":"group.updated","group":group}));
                }
            });
        }
        let _ = self
            .events
            .send(json!({"type":"job.updated","job":safe_job(job)}));
    }
    fn emit_jobs(&self, jobs: Vec<Value>) {
        for job in jobs {
            self.emit_job(job);
        }
    }
    fn close_agent(
        &self,
        node: &str,
        generation: Option<&str>,
        code: u16,
        reason: &str,
        fatal: Option<&str>,
    ) -> bool {
        let removed = {
            let mut agents = self.agents.lock().unwrap();
            if agents.get(node).is_some_and(|connection| {
                generation.is_none_or(|generation| connection.generation == generation)
            }) {
                agents.remove(node)
            } else {
                None
            }
        };
        if let Some(connection) = removed {
            if let Some(error) = fatal {
                let _ = connection
                    .sender
                    .try_send(json!({"type":"error","code":error,"message":reason,"fatal":true}));
            }
            connection.close.send_replace(Some(CloseNotice {
                code,
                reason: reason.into(),
            }));
            self.disconnected
                .lock()
                .unwrap()
                .insert(node.into(), state::now());
            true
        } else {
            false
        }
    }
    fn close_token(&self, token: &str) {
        let agents = self
            .agents
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, connection)| connection.token_id == token)
            .map(|(node, connection)| (node.clone(), connection.generation.clone()))
            .collect::<Vec<_>>();
        for (node, generation) in agents {
            self.close_agent(
                &node,
                Some(&generation),
                4001,
                "credential revoked",
                Some("UNAUTHORIZED"),
            );
        }
        for (_, close) in self
            .client_sessions
            .lock()
            .unwrap()
            .values()
            .filter(|(id, _)| id == token)
        {
            close.send_replace(true);
        }
    }
    async fn cancel_job(
        self: &Arc<Self>,
        job_id: &str,
        actor: &str,
        reason: &str,
    ) -> Result<Value> {
        let _coordination = self.lifecycle.lock().await;
        let id = job_id.to_owned();
        let actor = actor.to_owned();
        let reason = reason.to_owned();
        let online = self.online();
        let (changed, jobs) = self
            .db
            .write(move |c| {
                let mut job = db::get(c, "jobs", &id)?.context("NOT_FOUND: job")?;
                if job["state"] == "cancelled" {
                    return Ok((job, vec![]));
                }
                ensure!(
                    !protocol::terminal(job["state"].as_str().unwrap()),
                    "CONFLICT: job is already terminal"
                );
                let mut changed = vec![];
                if matches!(job["state"].as_str(), Some("created" | "queued")) {
                    changed.extend(finish_job(
                        c,
                        &mut job,
                        "cancelled",
                        &actor,
                        Some("cancelled before it started"),
                    )?);
                } else {
                    if job["cancelRequestedAt"].is_null() {
                        job["cancelRequestedAt"] = json!(state::now());
                        job["stateReason"] = json!("cancelling");
                        db::put(c, "jobs", &id, &job)?;
                        record_event(c, &job, "cancel-requested", &actor, None, None, None)?;
                        changed.push(job.clone());
                    }
                    if !job["nodeId"]
                        .as_str()
                        .is_some_and(|node| online.contains(node))
                    {
                        job["error"] = Value::Null;
                        changed.extend(finish_job(
                            c,
                            &mut job,
                            "cancelled",
                            &actor,
                            Some("node offline; the agent will clean up when it reconnects"),
                        )?);
                    }
                }
                refresh_nodes(c, &online)?;
                Ok((job, changed))
            })
            .await?;
        if dispatched(&changed)
            && let Some(node) = changed["nodeId"].as_str()
        {
            self.send_agent(
                node,
                json!({"type":"job.cancel","jobId":changed["id"],"reason":reason}),
            );
        }
        self.emit_jobs(jobs);
        Ok(safe_job(changed))
    }
    pub async fn sweep(self: &Arc<Self>) -> Result<()> {
        self.sweep_at(state::now()).await
    }
    /// The explicit clock makes lifecycle reconciliation deterministic in failure-injection tests.
    pub async fn sweep_at(self: &Arc<Self>, now: i64) -> Result<()> {
        let _coordination = self.lifecycle.lock().await;
        let connections = self
            .agents
            .lock()
            .unwrap()
            .iter()
            .map(|(node, connection)| (node.clone(), connection.clone()))
            .collect::<Vec<_>>();
        let tokens = connections
            .iter()
            .map(|(_, connection)| connection.token_id.clone())
            .chain(
                self.client_sessions
                    .lock()
                    .unwrap()
                    .values()
                    .map(|(token, _)| token.clone()),
            )
            .collect::<HashSet<_>>();
        let expired = self
            .db
            .call(move |c| {
                let mut expired = HashSet::new();
                for token in tokens {
                    if db::get(c, "tokens", &token)?.is_none_or(|record| {
                        !record["revokedAt"].is_null()
                            || record["expiresAt"]
                                .as_i64()
                                .is_some_and(|expiry| expiry <= now)
                    }) {
                        expired.insert(token);
                    }
                }
                Ok(expired)
            })
            .await?;
        for token in expired {
            self.close_token(&token);
        }
        for (node, connection) in connections {
            if now - connection.last_message_at > self.timing.heartbeat_timeout_ms {
                self.close_agent(
                    &node,
                    Some(&connection.generation),
                    4000,
                    "heartbeat timeout",
                    None,
                );
            }
        }
        let online = self.online();
        let disconnected = self.disconnected.lock().unwrap().clone();
        let timing = self.timing.clone();
        let started = self.started_at;
        let (jobs, cancels, nodes) = self
            .db
            .write(move |c| {
                let mut changed = vec![];
                let mut cancels = vec![];
                let names = db::list(c, "nodes")?
                    .into_iter()
                    .map(|node| {
                        (
                            node["id"].as_str().unwrap().to_owned(),
                            node["name"].as_str().unwrap_or("node").to_owned(),
                        )
                    })
                    .collect::<HashMap<_, _>>();
                for mut job in db::list(c, "jobs")? {
                    if !dispatched(&job) {
                        continue;
                    }
                    let id = job["id"].as_str().unwrap().to_owned();
                    let node = job["nodeId"].as_str().unwrap_or("").to_owned();
                    let name = names.get(&node).cloned().unwrap_or_else(|| "node".into());
                    if job["cancelRequestedAt"]
                        .as_i64()
                        .is_some_and(|at| now - at > timing.cancel_timeout_ms)
                    {
                        job["error"] = Value::Null;
                        job["finishedAt"] = json!(now);
                        changed.extend(finish_job(
                            c,
                            &mut job,
                            "cancelled",
                            "controller",
                            Some("the agent did not confirm cancellation in time"),
                        )?);
                        continue;
                    }
                    let lost = if let Some(at) = disconnected.get(&node) {
                        now - at > timing.lost_timeout_ms
                    } else {
                        now - started > timing.startup_grace_ms
                    };
                    if !online.contains(&node) && lost {
                        if job["state"] == "assigned" && job["cancelRequestedAt"].is_null() {
                            requeue(
                                c,
                                &mut job,
                                &format!("{name} went offline before starting the job"),
                                "controller",
                            )?;
                            changed.push(job);
                        } else if !job["cancelRequestedAt"].is_null() {
                            job["error"] = Value::Null;
                            job["finishedAt"] = json!(now);
                            changed.extend(finish_job(
                                c,
                                &mut job,
                                "cancelled",
                                "controller",
                                Some(&format!("{name} is offline")),
                            )?);
                        } else {
                            job["error"] = json!(format!("Lost contact with {name}"));
                            job["finishedAt"] = json!(now);
                            changed.extend(finish_job(
                                c,
                                &mut job,
                                "lost",
                                "controller",
                                Some(&format!("{name} has been offline")),
                            )?);
                        }
                        continue;
                    }
                    if job["state"] == "assigned"
                        && job["cancelRequestedAt"].is_null()
                        && online.contains(&node)
                        && job["assignedAt"]
                            .as_i64()
                            .is_some_and(|at| now - at > timing.assign_timeout_ms)
                    {
                        cancels.push((node.clone(), id));
                        requeue(
                            c,
                            &mut job,
                            &format!("{name} did not acknowledge the job"),
                            "controller",
                        )?;
                        changed.push(job);
                    }
                }
                for record in db::list(c, "rejections")? {
                    let id = record["jobId"]
                        .as_str()
                        .context("rejection job ID")?
                        .to_owned();
                    let mut record = record;
                    record["nodes"]
                        .as_object_mut()
                        .context("rejection map")?
                        .retain(|_, expiry| expiry.as_i64().is_some_and(|expiry| expiry > now));
                    if record["nodes"].as_object().unwrap().is_empty() {
                        db::delete(c, "rejections", &id)?;
                    } else {
                        db::put(c, "rejections", &id, &record)?;
                    }
                }
                let nodes = refresh_nodes(c, &online)?;
                Ok((changed, cancels, nodes))
            })
            .await?;
        for (node, job) in cancels {
            self.send_agent(
                &node,
                json!({"type":"job.cancel","jobId":job,"reason":"assignment timed out"}),
            );
        }
        self.emit_jobs(jobs);
        for node in nodes {
            let _ = self.events.send(json!({"type":"node.updated","node":node}));
        }
        Ok(())
    }
    /// Apply the retained policy and mark/sweep only old, unreferenced regular CAS files.
    pub async fn run_retention_at(self: &Arc<Self>, now: i64) -> Result<Value> {
        let cas_guard = Arc::new(self.cas_lifecycle.clone().write_owned().await);
        let (mut summary,snapshots,mut live)=self.db.write(move|c|{
            let settings=cluster_settings(c)?;let day=86_400_000i64;
            let mut jobs_deleted=0;let mut logs_deleted=0;let mut snapshots_deleted=0;
            for job in db::list(c,"jobs")?{let id=job["id"].as_str().context("stored job ID")?;
                if protocol::terminal(job["state"].as_str().unwrap_or(""))&&job["finishedAt"].as_i64().unwrap_or(0)<now-settings["jobRetentionDays"].as_i64().unwrap_or(90)*day{
                    db::delete(c,"jobs",id)?;db::delete(c,"rejections",id)?;c.execute("DELETE FROM logs WHERE job_id=?1",[id])?;for event in db::list(c,"events")?{if event["jobId"]==id{db::delete(c,"events",&format!("{:020}",event["id"].as_u64().unwrap_or(0)))?;}}jobs_deleted+=1;
                }else if protocol::terminal(job["state"].as_str().unwrap_or(""))&&job["finishedAt"].as_i64().unwrap_or(0)<now-settings["logRetentionDays"].as_i64().unwrap_or(30)*day&&c.execute("DELETE FROM logs WHERE job_id=?1",[id])?>0{logs_deleted+=1;}
            }
            let jobs=db::list(c,"jobs")?;for group in db::list(c,"groups")?{if !jobs.iter().any(|job|job["groupId"]==group["id"]){db::delete(c,"groups",group["id"].as_str().unwrap())?;}}
            for snapshot in db::list(c,"snapshots")?{let id=snapshot["id"].as_str().context("stored snapshot ID")?;
                if snapshot["lastUsedAt"].as_i64().or(snapshot["createdAt"].as_i64()).unwrap_or(0)<now-settings["snapshotRetentionDays"].as_i64().unwrap_or(7)*day&&!jobs.iter().any(|job|!protocol::terminal(job["state"].as_str().unwrap_or(""))&&job["snapshotId"]==id){db::delete(c,"snapshots",id)?;snapshots_deleted+=1;}
            }
            let live=jobs.iter().flat_map(|job|job["artifacts"].as_array().into_iter().flatten()).filter_map(|artifact|artifact["digest"].as_str().map(str::to_owned)).collect::<HashSet<_>>();
            Ok((json!({"jobsDeleted":jobs_deleted,"logsDeleted":logs_deleted,"snapshotsDeleted":snapshots_deleted,"blobsDeleted":0,"bytesFreed":0}),db::list(c,"snapshots")?,live))
        }).await?;
        let directory = self.dir.clone();
        let instance = self._instance.clone();
        let cas_work = cas_guard.clone();
        let (deleted, bytes, corrupt) = tokio::task::spawn_blocking(move || -> Result<_> {
            let _instance = instance;
            let _cas = cas_work;
            let store = crate::workspace::BlobStore::new(directory.join("blobs"));
            let mut corrupt = vec![];
            for snapshot in snapshots {
                let id = snapshot["id"].as_str().context("stored snapshot ID")?;
                live.insert(id.to_owned());
                let manifest = store
                    .read(id)
                    .and_then(|bytes| {
                        Ok(serde_json::from_slice::<crate::workspace::Manifest>(
                            &bytes,
                        )?)
                    })
                    .and_then(|manifest| {
                        manifest.validate()?;
                        Ok(manifest)
                    });
                match manifest {
                    Ok(manifest) => {
                        for entry in manifest.entries {
                            if let crate::workspace::ManifestEntry::File(file) = entry {
                                live.insert(file.digest);
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("snapshot {id} unreadable during retention: {error}");
                        corrupt.push(id.to_owned());
                    }
                }
            }
            let base = store.root().join("sha256");
            let mut deleted = 0u64;
            let mut bytes = 0u64;
            if base.exists() {
                for prefix in std::fs::read_dir(base)? {
                    let prefix = prefix?;
                    if !prefix.file_type()?.is_dir() {
                        continue;
                    }
                    let prefix_name = prefix.file_name().to_string_lossy().into_owned();
                    if prefix_name.len() != 2 {
                        continue;
                    }
                    for entry in std::fs::read_dir(prefix.path())? {
                        let entry = entry?;
                        if !entry.file_type()?.is_file() {
                            continue;
                        }
                        let digest = format!(
                            "sha256:{prefix_name}{}",
                            entry.file_name().to_string_lossy()
                        );
                        if crate::workspace::validate_digest(&digest).is_err()
                            || live.contains(&digest)
                        {
                            continue;
                        }
                        let metadata = entry.metadata()?;
                        let modified = metadata
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis()
                            .min(i64::MAX as u128) as i64;
                        if now.saturating_sub(modified) < 6 * 3_600_000 {
                            continue;
                        }
                        std::fs::remove_file(entry.path())?;
                        deleted += 1;
                        bytes = bytes.saturating_add(metadata.len());
                    }
                }
            }
            Ok((deleted, bytes, corrupt))
        })
        .await??;
        if !corrupt.is_empty() {
            let count = corrupt.len();
            self.db
                .write(move |c| {
                    for id in corrupt {
                        db::delete(c, "snapshots", &id)?;
                    }
                    Ok(())
                })
                .await?;
            summary["snapshotsDeleted"] =
                json!(summary["snapshotsDeleted"].as_u64().unwrap_or(0) + count as u64);
        }
        summary["blobsDeleted"] = json!(deleted);
        summary["bytesFreed"] = json!(bytes);
        Ok(summary)
    }
    async fn schedule(self: &Arc<Self>) -> Result<()> {
        let _coordination = self.lifecycle.lock().await;
        let connections = self
            .agents
            .lock()
            .unwrap()
            .iter()
            .map(|(id, connection)| (id.clone(), connection.sender.clone()))
            .collect::<HashMap<_, _>>();
        let available = connections.keys().cloned().collect::<HashSet<_>>();
        let now = state::now();
        let (assignments, changed, nodes) = self
            .db
            .write(move |c| {
                refresh_nodes(c, &available)?;
                let mut nodes = db::list(c, "nodes")?;
                let jobs = db::list(c, "jobs")?;
                for node in &mut nodes {
                    let groups = jobs
                        .iter()
                        .filter(|job| job["nodeId"] == node["id"] && dispatched(job))
                        .filter_map(|job| job["groupId"].as_str())
                        .fold(serde_json::Map::new(), |mut groups, group| {
                            let count = groups.get(group).and_then(Value::as_u64).unwrap_or(0);
                            groups.insert(group.into(), json!(count + 1));
                            groups
                        });
                    node["groups"] = json!(groups);
                    if node["updateHold"] == true {
                        node["settings"]["schedulingMode"] = json!("draining");
                    }
                }
                let mut queued = jobs
                    .iter()
                    .filter(|job| job["state"] == "queued")
                    .cloned()
                    .collect::<Vec<_>>();
                for job in &mut queued {
                    if let Some(rejections) = db::get(c, "rejections", job["id"].as_str().unwrap())?
                    {
                        job["excludedNodes"] = json!(
                            rejections["nodes"]
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter(|(_, expiry)| expiry
                                    .as_i64()
                                    .is_some_and(|expiry| expiry > now))
                                .map(|(node, _)| node.clone())
                                .collect::<Vec<_>>()
                        );
                    }
                }
                let mut context = crate::scheduler::ScheduleContext::default();
                for job in &jobs {
                    if job["state"] == "succeeded"
                        && job["finishedAt"]
                            .as_i64()
                            .is_some_and(|at| now - at < 24 * 3_600_000)
                        && let (Some(project), Some(node)) =
                            (job["spec"]["project"].as_str(), job["nodeId"].as_str())
                    {
                        context
                            .warm_nodes
                            .entry(project.into())
                            .or_default()
                            .insert(node.into());
                    }
                }
                let plan = crate::scheduler::schedule(&queued, &nodes, &context);
                let mut assignments = vec![];
                let mut changed = vec![];
                for assignment in plan.assignments {
                    let mut job =
                        db::get(c, "jobs", &assignment.job_id)?.context("job disappeared")?;
                    let node =
                        db::get(c, "nodes", &assignment.node_id)?.context("node disappeared")?;
                    job["nodeId"] = json!(assignment.node_id);
                    job["nodeName"] = node["name"].clone();
                    job["assignmentId"] = json!(state::id());
                    job["assignedAt"] = json!(now);
                    transition(c, &mut job, "assigned", None)?;
                    assignments.push(job);
                }
                let mut pending = queued;
                pending.sort_by(|a, b| {
                    b["priority"]
                        .as_i64()
                        .cmp(&a["priority"].as_i64())
                        .then(a["queuedAt"].as_i64().cmp(&b["queuedAt"].as_i64()))
                        .then(a["id"].as_str().cmp(&b["id"].as_str()))
                });
                let mut position = 0;
                for job in pending {
                    let id = job["id"].as_str().unwrap();
                    if let Some(reason) = plan.pending.get(id)
                        && let Some(mut current) = db::get(c, "jobs", id)?
                    {
                        position += 1;
                        if current["stateReason"] != *reason || current["queuePosition"] != position
                        {
                            current["stateReason"] = json!(reason);
                            current["queuePosition"] = json!(position);
                            db::put(c, "jobs", id, &current)?;
                            changed.push(current);
                        }
                    }
                }
                Ok((assignments, changed, refresh_nodes(c, &available)?))
            })
            .await?;
        for job in assignments {
            let delivered=connections.get(job["nodeId"].as_str().unwrap()).is_some_and(|sender|sender.try_send(json!({"type":"job.assign","jobId":job["id"],"assignmentId":job["assignmentId"],"attempt":job["attempt"],"spec":job["spec"],"snapshotId":job["snapshotId"]})).is_ok());
            if delivered {
                self.emit_job(job);
            } else {
                let id = job["id"].as_str().unwrap().to_owned();
                let assignment = job["assignmentId"].clone();
                let changed = self
                    .db
                    .write(move |c| {
                        let mut job = db::get(c, "jobs", &id)?.context("job disappeared")?;
                        if job["state"] == "assigned" && job["assignmentId"] == assignment {
                            requeue(c, &mut job, "worker channel unavailable", "controller")?;
                        }
                        Ok(job)
                    })
                    .await?;
                self.emit_job(changed);
            }
        }
        self.emit_jobs(changed);
        for node in nodes {
            let _ = self.events.send(json!({"type":"node.updated","node":node}));
        }
        Ok(())
    }
}

/// Start with production timing. Each listener and periodic worker is owned by this future;
/// cancelling a foreground controller also cancels its background work.
pub async fn start(dir: PathBuf, host: Option<String>, port: Option<u16>) -> Result<()> {
    start_with_timing(dir, host, port, ControllerTiming::default()).await
}
pub async fn start_with_timing(
    dir: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    timing: ControllerTiming,
) -> Result<()> {
    start_controlled(
        dir,
        host,
        port,
        timing,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}
struct StopControllerListeners {
    shutdown: watch::Sender<bool>,
    handles: Vec<axum_server::Handle>,
}
impl Drop for StopControllerListeners {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        for handle in &self.handles {
            handle.shutdown();
        }
    }
}
struct CancelControllerUpdates(tokio_util::sync::CancellationToken);
impl Drop for CancelControllerUpdates {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
async fn controller_updates<F, Fut>(
    controller: Shared,
    cancel: tokio_util::sync::CancellationToken,
    update: F,
) -> Result<()>
where
    F: Fn(Shared, tokio_util::sync::CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut timer = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {biased;_=cancel.cancelled()=>return Ok(()),_=timer.tick()=>{}}
        if let Err(error) = update(controller.clone(), cancel.clone()).await
            && !cancel.is_cancelled()
        {
            eprintln!("update coordination: {error}");
        }
    }
}
async fn start_controlled(
    dir: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    timing: ControllerTiming,
    stop: tokio_util::sync::CancellationToken,
) -> Result<()> {
    start_controlled_with_updates(
        dir,
        host,
        port,
        timing,
        stop,
        crate::updates::tick_with_cancel,
    )
    .await
}
async fn start_controlled_with_updates<F, Fut>(
    dir: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    timing: ControllerTiming,
    stop: tokio_util::sync::CancellationToken,
    update: F,
) -> Result<()>
where
    F: Fn(Shared, tokio_util::sync::CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let controller = Controller::open_with_timing(dir.clone(), timing.clone()).await?;
    controller
        .db
        .write(|c| {
            for mut node in db::list(c, "nodes")? {
                node["status"] = json!("offline");
                node["metrics"] = Value::Null;
                db::put(c, "nodes", node["id"].as_str().unwrap(), &node)?;
            }
            Ok(())
        })
        .await?;
    let app=Router::new().route("/v1/health",get(||async{Json(json!({"ok":true,"service":"gridstator-controller","version":crate::VERSION,"apiVersion":"v1","native":true}))}))
        .route("/v1/pki/ca.pem",get(ca)).route("/v1/agent/ws",get(agent_upgrade)).route("/v1/events",get(events_upgrade)).route("/v1/blobs/upload",post(blob_upload)).route("/v1/controller/build",post(controller_publish)).fallback(api).layer(axum::extract::DefaultBodyLimit::max(64*1024*1024)).with_state(controller.clone());
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        std::fs::read(dir.join("pki/server.pem"))?,
        std::fs::read(dir.join("pki/server.key"))?,
    )
    .await?;
    let host = host.unwrap_or_else(|| {
        controller.config["host"]
            .as_str()
            .unwrap_or("0.0.0.0")
            .into()
    });
    let port = port.unwrap_or(controller.config["port"].as_u64().unwrap_or(7300) as u16);
    let mut listeners = vec![std::net::TcpListener::bind((host.as_str(), port))?];
    let primary = listeners[0].local_addr()?;
    if let Some(aliases) = controller.config["aliases"].as_array() {
        for alias in aliases {
            let host = alias["host"].as_str().context("alias host is required")?;
            let port = alias["port"]
                .as_u64()
                .filter(|port| *port <= u16::MAX as u64)
                .context("invalid alias port")? as u16;
            if host == primary.ip().to_string() && port == primary.port() {
                continue;
            }
            listeners.push(
                std::net::TcpListener::bind((host, port))
                    .with_context(|| format!("bind controller alias {host}:{port}"))?,
            );
        }
    }
    let addresses = listeners
        .iter()
        .map(std::net::TcpListener::local_addr)
        .collect::<std::io::Result<Vec<_>>>()?;
    state::write_json(
        &dir.join("runtime.json"),
        &json!({"pid":std::process::id(),"host":primary.ip().to_string(),"port":primary.port(),"aliases":addresses.iter().skip(1).map(|address|json!({"host":address.ip().to_string(),"port":address.port()})).collect::<Vec<_>>(),"startedAt":controller.started_at}),
    )?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut servers = StopControllerListeners {
        shutdown: controller.shutdown.clone(),
        handles: vec![],
    };
    for listener in listeners {
        let handle = axum_server::Handle::new();
        servers.handles.push(handle.clone());
        let app = app.clone();
        let tls = tls.clone();
        tasks.spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .handle(handle)
                .serve(app.into_make_service())
                .await
                .map_err(anyhow::Error::from)
        });
    }
    // Cancel transport only; join started backup/install/activation work before startup can recur.
    let update_cancel = tokio_util::sync::CancellationToken::new();
    let update_lifetime = CancelControllerUpdates(update_cancel.clone());
    let mut updater = tokio::spawn(controller_updates(
        controller.clone(),
        update_cancel,
        update,
    ));
    let monitor = controller.clone();
    let maintenance_stop = update_lifetime.0.clone();
    tasks.spawn(async move{let mut sweep=tokio::time::interval(timing.sweep_interval);let mut schedule=tokio::time::interval(timing.schedule_interval);let mut retention=tokio::time::interval(timing.retention_interval);loop{tokio::select!{biased;
        _=maintenance_stop.cancelled()=>break,
        _=sweep.tick()=>{if let Err(error)=monitor.sweep().await{eprintln!("controller sweep: {error}");}},
        _=schedule.tick()=>{if let Err(error)=monitor.schedule().await{eprintln!("controller schedule: {error}");}},
        _=retention.tick()=>{if let Err(error)=monitor.run_retention_at(state::now()).await{eprintln!("controller retention: {error}");}}
    }}#[allow(unreachable_code)] Ok(())});
    let confirmation_dir = dir.clone();
    let confirmation_instance = controller._instance.clone();
    let local_ip = if primary.ip().is_unspecified() {
        if primary.is_ipv6() {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        } else {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        }
    } else {
        primary.ip()
    };
    let local_url = format!(
        "https://{}",
        std::net::SocketAddr::new(local_ip, primary.port())
    );
    tasks.spawn(async move {
        let ca = std::fs::read_to_string(confirmation_dir.join("pki/ca.pem"))?;
        let client = crate::pki::http_client(&ca)?;
        for _ in 0..100 {
            if client
                .get(format!("{local_url}/v1/health"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                tokio::task::spawn_blocking(move || {
                    // Confirmation verifies executable hashes and atomically refreshes the
                    // launcher. Keep that I/O off the reactor and retain this instance's
                    // ownership if shutdown cancels the awaiting health task.
                    let _instance = confirmation_instance;
                    crate::updates::confirm_controller(&confirmation_dir)
                })
                .await??;
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("controller health confirmation timed out")
    });
    eprintln!("Controller listening on https://{primary}");
    let outcome = loop {
        tokio::select! {
            _=state::shutdown_signal()=>break Ok(()),_=controller.restart.notified()=>break Ok(()),_=stop.cancelled()=>break Ok(()),
            result=&mut updater=>break result.map_err(anyhow::Error::from).and_then(|result|result),
            result=tasks.join_next()=>{match result{Some(Ok(Ok(())))=>continue,Some(Ok(Err(error)))=>break Err(error),Some(Err(error))=>break Err(error.into()),None=>break Ok(())}}
        }
    };
    controller.shutdown.send_replace(true);
    for handle in &servers.handles {
        handle.graceful_shutdown(Some(Duration::from_secs(10)));
    }
    update_lifetime.0.cancel();
    // The detached updater also retains controller ownership if this outer future is aborted.
    if !updater.is_finished()
        && let Err(error) = updater.await
    {
        eprintln!("controller updater shutdown: {error}");
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let _ = std::fs::remove_file(dir.join("runtime.json"));
    outcome
}
pub async fn start_embedded(
    dir: PathBuf,
    host: Option<String>,
    port: Option<u16>,
    name: Option<String>,
    settings: Option<Value>,
) -> Result<()> {
    let _ = std::fs::remove_file(dir.join("runtime.json"));
    let server_stop = tokio_util::sync::CancellationToken::new();
    let mut server = tokio::spawn(start_controlled(
        dir.clone(),
        host,
        port,
        ControllerTiming::default(),
        server_stop.clone(),
    ));
    let worker_dir = dir.join("embedded-agent");
    if !worker_dir.join("agent.json").exists() {
        let runtime = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(runtime) = state::read_json(&dir.join("runtime.json"))
                    && runtime["pid"] == std::process::id()
                {
                    break runtime;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("embedded controller did not bind its listener")?;
        let port = runtime["port"]
            .as_u64()
            .context("controller runtime port missing")?;
        let host = runtime["host"].as_str().unwrap_or("127.0.0.1");
        let url = listener_url(host, port);
        let database = Db::open(&dir.join("controller.db"))?;
        let token = database
            .write(|c| {
                issue_token(
                    c,
                    "join",
                    "embedded-worker",
                    "operator",
                    None,
                    Some(1),
                    Some(state::now() + 60000),
                )
            })
            .await?;
        let ca = std::fs::read_to_string(dir.join("pki/ca.pem"))?;
        let client = crate::pki::http_client(&ca)?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.get(format!("{url}/v1/health")).send().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?;
        crate::worker::join(
            &worker_dir,
            &url,
            token["token"].as_str().unwrap(),
            &crate::pki::fingerprint(&ca)?,
            name,
            settings,
        )
        .await?;
    }
    let supervisor = crate::worker::control::EmbeddedSupervisor::bind(&dir, server_stop.clone())?;
    let mut should_start = crate::worker::policy::load(&worker_dir.join("local-policy.json"))?
        .start_at_sign_in
        && !crate::local_worker::manually_stopped(&worker_dir)?;
    loop {
        if !should_start {
            tokio::select! {
                result = &mut server => { result??; return Ok(()); },
                _ = supervisor.start.notified() => {},
            }
        }
        should_start = false;
        let worker_stop = tokio_util::sync::CancellationToken::new();
        let hook: crate::worker::UpdateHook =
            Arc::new(|check| Box::pin(crate::updates::worker_update(check)));
        let mut agent = tokio::spawn(crate::worker::run_with_shutdown(
            worker_dir.clone(),
            crate::worker::WorkerOptions::default(),
            worker_stop.clone(),
            Some(hook),
        ));
        tokio::select! {
            result = &mut server => { worker_stop.cancel(); let cleanup=agent.await; result??; cleanup??; return Ok(()); },
            result = &mut agent => {
                match result? {
                    Ok(crate::worker::WorkerExit::LocalStopped) => {
                        // A local Stop holds only this worker. Keep the controller and the private
                        // Start endpoint alive; the existing registration is reused on Start.
                        continue;
                    },
                    outcome => {
                        // Managed update/fatal/external exits retain the established coordinated
                        // controller maintenance path rather than becoming a manual Stop.
                        server_stop.cancel(); let cleanup=server.await; cleanup??; outcome?; return Ok(());
                    },
                }
            },
        }
    }
}
async fn ca(State(s): State<Shared>) -> std::result::Result<String, ApiError> {
    Ok(std::fs::read_to_string(s.dir.join("pki/ca.pem")).map_err(anyhow::Error::from)?)
}

async fn api(
    State(s): State<Shared>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> std::result::Result<Response, ApiError> {
    api_inner(s, method, headers, uri, body)
        .await
        .map_err(ApiError)
}
async fn api_inner(
    s: Shared,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response> {
    let path = uri.path().to_owned();
    let register = path == "/v1/agents/register";
    let blobs = path.starts_with("/v1/blobs/");
    let admin = path.starts_with("/v1/tokens")
        || (method != Method::GET && (path == "/v1/settings" || path.contains("updates")))
        || (method == Method::DELETE && path.starts_with("/v1/nodes"));
    let principal = s
        .principal(
            &headers,
            if path.starts_with("/v2/agent/") {
                &["agent"]
            } else if register {
                &["join"]
            } else if blobs
                || (method == Method::GET && path.starts_with("/v1/snapshots/"))
                || path.starts_with("/v2/agent/")
            {
                &["client", "agent"]
            } else {
                &["client"]
            },
            admin,
        )
        .await?;
    if blobs {
        if method == Method::GET {
            let digest =
                decode_path_component(path.trim_start_matches("/v1/blobs/")).map_err(|error| {
                    anyhow::anyhow!("BAD_REQUEST: invalid blob digest path: {error}")
                })?;
            let store = crate::workspace::BlobStore::new(s.dir.join("blobs"));
            let file = store
                .open(&digest)
                .map_err(|error| anyhow::anyhow!("NOT_FOUND: blob: {error}"))?;
            let size = file.metadata()?.len();
            return Ok(Response::builder()
                .header("content-type", "application/octet-stream")
                .header("content-length", size)
                .body(Body::from_stream(tokio_util::io::ReaderStream::new(
                    tokio::fs::File::from_std(file),
                )))?);
        }
        ensure!(method == Method::POST, "NOT_FOUND: blob route");
        return blob_api(&s, &path, &body).await;
    }
    if method == Method::GET && path.starts_with("/v1/jobs/") {
        let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
        if parts.len() >= 5 && parts[3] == "artifacts" {
            let id = parts[2].to_owned();
            let name = decode_path_component(&parts[4..].join("/"))?;
            let job =
                s.db.call(move |c| db::get(c, "jobs", &id))
                    .await?
                    .context("NOT_FOUND: job")?;
            let artifact = job["artifacts"]
                .as_array()
                .context("NOT_FOUND: artifacts")?
                .iter()
                .find(|a| a["name"] == name)
                .context("NOT_FOUND: artifact")?;
            let store = crate::workspace::BlobStore::new(s.dir.join("blobs"));
            let file = tokio::fs::File::open(
                store.path(
                    artifact["digest"]
                        .as_str()
                        .context("invalid artifact digest")?,
                )?,
            )
            .await
            .map_err(|_| anyhow::anyhow!("NOT_FOUND: artifact blob"))?;
            return Ok(Response::builder()
                .header("content-type", "application/octet-stream")
                .header("content-length", artifact["size"].as_u64().unwrap_or(0))
                .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))?);
        }
    }
    let mut input: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(anyhow::Error::from)?
    };
    protocol::canonicalize_numbers(&mut input);
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if let Some(response) = crate::updates::route(
        s.clone(),
        method.clone(),
        &path,
        &query,
        input.clone(),
        principal.clone(),
    )
    .await?
    {
        return Ok(response);
    }
    let owner = principal["name"].as_str().unwrap_or("client").to_owned();
    let output = match (method.as_str(), path.as_str()) {
        ("GET", "/v1/cluster") => {
            let nodes = s.db.call(|c| db::list(c, "nodes")).await?;
            let jobs = s.db.call(|c| db::list(c, "jobs")).await?;
            let ca =
                std::fs::read_to_string(s.dir.join("pki/ca.pem")).map_err(anyhow::Error::from)?;
            let directory = s.dir.clone();
            let build = tokio::task::spawn_blocking(move || {
                crate::updates::controller_build_view(&directory)
            })
            .await?;
            let mut counts = serde_json::Map::new();
            for job in jobs {
                let state = job["state"].as_str().unwrap_or("lost");
                let count = counts.get(state).and_then(Value::as_u64).unwrap_or(0);
                counts.insert(state.into(), json!(count + 1));
            }
            json!({"clusterId":s.config["clusterId"],"version":crate::VERSION,"build":build,"controller":{"hostname":sysinfo::System::host_name().unwrap_or_default(),"machineId":crate::worker::machine_id()},"caFingerprint":crate::pki::fingerprint(&ca)?,"startedAt":s.started_at,"nodes":{"total":nodes.len(),"online":nodes.iter().filter(|n|n["status"]=="online").count()},"jobs":counts,"whoami":{"name":principal["name"],"role":principal["role"]}})
        }
        ("GET", "/v1/nodes") => {
            let online = s.online();
            json!({"nodes":s.db.call(move|c|db::list(c,"nodes")?.into_iter().map(|node|{let connected=online.contains(node["id"].as_str().unwrap());node_view(c,node,connected)}).collect::<Result<Vec<_>>>()).await?})
        }
        ("POST", "/v1/agents/register") => {
            let token_id = principal["id"].as_str().unwrap().to_owned();
            let name = input["name"]
                .as_str()
                .context("name is required")?
                .to_owned();
            let info = input["info"].clone();
            validate_node_info(&info)?;
            validate_node_name(&name)?;
            let cluster_id = s.config["clusterId"].clone();
            s.db.write(move|c|{
                ensure!(!db::list(c,"nodes")?.iter().any(|n|n["name"]==name||n["id"]==name),"CONFLICT: node name already exists or conflicts with a node ID");
                let id=state::id();let cpus=info["cpus"].as_u64().unwrap_or(1);let memory=info["memoryTotalMb"].as_u64().unwrap_or(4096);
                let settings=merge_node_settings(json!({"maxConcurrency":(cpus/2).clamp(1,4),"reserved":{"cpus":if cpus>=4{1}else{0},"memoryMb":(memory/10).min(4096)},"labels":[],"schedulingMode":"enabled","autoUpdate":"inherit"}),&input["settings"])?;
                let node=json!({"id":id,"name":name,"status":"offline","health":"unknown","healthIssues":[],"info":info,"metrics":null,"settings":settings,"effectiveLabels":[],"activeJobs":[],"allocated":{"cpus":0,"memoryMb":0},"allocatable":{"cpus":cpus,"memoryMb":memory},"lastSeenAt":null,"connectedAt":null,"createdAt":state::now()});db::put(c,"nodes",&id,&node)?;
                let mut token=db::get(c,"tokens",&token_id)?.context("UNAUTHORIZED: join token vanished")?;ensure!(token["kind"]=="join"&&token["revokedAt"].is_null()&&token["expiresAt"].as_i64().is_none_or(|expiry|expiry>state::now()),"UNAUTHORIZED: expired or revoked join token");if let Some(uses)=token["usesRemaining"].as_u64(){ensure!(uses>0,"UNAUTHORIZED: exhausted join token");token["usesRemaining"]=json!(uses-1);db::put(c,"tokens",&token_id,&token)?;}
                let credential=issue_token(c,"agent",&name,"agent",Some(&id),None,None)?;
                Ok(json!({"nodeId":id,"name":name,"credential":credential["token"],"clusterId":cluster_id}))
            }).await?
        }
        ("POST", "/v1/jobs") => {
            let spec = protocol::normalize_spec(input["spec"].clone())?;
            let job =
                s.db.write(move |c| create_job(c, spec, &owner, None, None))
                    .await?;
            s.emit_job(job.clone());
            let _ = s.schedule().await;
            return Ok((StatusCode::CREATED, Json(safe_job(job))).into_response());
        }
        ("GET", "/v1/jobs") => {
            let mut jobs = s.db.call(|c| db::list(c, "jobs")).await?;
            jobs.reverse();
            jobs.retain(|j| {
                query
                    .get("state")
                    .is_none_or(|states| states.split(',').any(|x| j["state"] == x))
                    && query
                        .get("node")
                        .is_none_or(|n| j["nodeId"] == n.as_str() || j["nodeName"] == n.as_str())
                    && query
                        .get("before")
                        .is_none_or(|id| j["id"].as_str().unwrap_or("") < id.as_str())
            });
            let limit = query
                .get("limit")
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(100);
            ensure!((1..=500).contains(&limit), "limit must be 1..500");
            jobs.truncate(limit);
            json!({"jobs":jobs.into_iter().map(safe_job).collect::<Vec<_>>()})
        }
        ("POST", "/v1/tokens") => {
            let kind = if input["kind"] == "join" || input["join"] == true {
                "join"
            } else {
                "client"
            };
            ensure!(
                input["kind"].is_null()
                    || matches!(input["kind"].as_str(), Some("client" | "join")),
                "invalid token kind"
            );
            let name = input["name"]
                .as_str()
                .context("token name required")?
                .to_owned();
            ensure!(
                !name.is_empty() && name.encode_utf16().count() <= 64,
                "token name must be 1..64 characters"
            );
            let role = input["role"].as_str().unwrap_or("operator").to_owned();
            ensure!(
                matches!(role.as_str(), "operator" | "admin"),
                "invalid token role"
            );
            let uses = if kind == "join" {
                let uses = input
                    .get("maxUses")
                    .or(input.get("uses"))
                    .map(|v| v.as_u64().context("maxUses must be positive"))
                    .transpose()?
                    .unwrap_or(1);
                ensure!((1..=1000).contains(&uses), "maxUses must be 1..1000");
                Some(uses)
            } else {
                None
            };
            let expires = if let Some(ttl) = input.get("ttlSeconds") {
                let ttl = ttl
                    .as_i64()
                    .filter(|ttl| *ttl > 0)
                    .context("ttlSeconds must be positive")?;
                Some(
                    state::now()
                        .checked_add(ttl.checked_mul(1000).context("ttlSeconds is too large")?)
                        .context("ttlSeconds is too large")?,
                )
            } else if let Some(expiry) = input["expiresAt"].as_i64() {
                Some(expiry)
            } else if kind == "join" {
                Some(state::now() + 86_400_000)
            } else {
                None
            };
            let mut issued =
                s.db.write(move |c| issue_token(c, kind, &name, &role, None, uses, expires))
                    .await?;
            let raw = issued.as_object_mut().unwrap().remove("token").unwrap();
            json!({"token":raw,"info":issued})
        }
        ("GET", "/v1/tokens") => {
            let mut tokens = s.db.call(|c| db::list(c, "tokens")).await?;
            for token in &mut tokens {
                token.as_object_mut().unwrap().remove("hash");
            }
            json!({"tokens":tokens})
        }
        ("GET", "/v1/settings") => s.db.call(|c| cluster_settings(c)).await?,
        ("PATCH", "/v1/settings") => {
            s.db.write(move |c| {
                let settings = patch_cluster_settings(cluster_settings(c)?, &input)?;
                db::put(c, "settings", "cluster", &settings)?;
                Ok(settings)
            })
            .await?
        }
        ("POST", "/v1/snapshots") => {
            let _cas = s.cas_lifecycle.read().await;
            let store = crate::workspace::BlobStore::new(s.dir.join("blobs"));
            let (id, manifest) = if let Some(id) = input["manifestDigest"].as_str() {
                ensure!(
                    store.has(id)?,
                    "NOT_FOUND: Manifest {id} has not been uploaded"
                );
                let manifest: crate::workspace::Manifest =
                    serde_json::from_slice(&store.read(id)?)?;
                (id.to_owned(), manifest)
            } else {
                let manifest: crate::workspace::Manifest =
                    serde_json::from_value(input["manifest"].clone())?;
                manifest.validate()?;
                let bytes = manifest.serialize()?;
                let id = manifest.id()?;
                store.put_bytes(&id, &bytes)?;
                (id, manifest)
            };
            manifest.validate()?;
            let mut bytes = 0u64;
            let mut count = 0u64;
            for entry in &manifest.entries {
                if let crate::workspace::ManifestEntry::File(file) = entry {
                    ensure!(store.has(&file.digest)?, "snapshot references missing blob");
                    bytes = bytes
                        .checked_add(file.size)
                        .context("snapshot size overflow")?;
                    count += 1;
                }
            }
            let serialized = serde_json::to_value(manifest)?;
            s.db.write(move|c|{let previous=db::get(c,"snapshots",&id)?;let snapshot=json!({"id":id,"manifest":serialized,"fileCount":count,"totalBytes":bytes,"createdAt":previous.as_ref().map(|v|v["createdAt"].clone()).unwrap_or(json!(state::now())),"lastUsedAt":state::now(),"owner":previous.as_ref().map(|v|v["owner"].clone()).unwrap_or(json!(owner))});db::put(c,"snapshots",&id,&snapshot)?;Ok(snapshot_view(snapshot,false))}).await?
        }
        ("GET", "/v1/groups") => {
            let before = query.get("before").cloned();
            let limit = query
                .get("limit")
                .map(|value| value.parse::<usize>())
                .transpose()?
                .unwrap_or(100);
            ensure!((1..=500).contains(&limit), "limit must be 1..500");
            json!({"groups":s.db.call(move|c|{let mut groups=db::list(c,"groups")?;groups.reverse();groups.retain(|group|before.as_ref().is_none_or(|before|group["id"].as_str().unwrap_or("")<before.as_str()));groups.truncate(limit);groups.iter().map(|group|group_view(c,group["id"].as_str().unwrap())).collect::<Result<Vec<_>>>()}).await?})
        }
        ("POST", "/v1/groups") => {
            let spec = protocol::normalize_spec(input["spec"].clone())?;
            ensure!(spec["kind"] == "command", "groups require command jobs");
            let total = input["shards"].as_u64().context("shards required")?;
            ensure!((1..=64).contains(&total), "shards must be 1..64");
            let group=s.db.write(move|c|{let id=state::id();let group=json!({"id":id,"name":input["name"].as_str().map(str::to_owned).unwrap_or_else(||display_name(&spec)),"owner":owner,"shardTotal":total,"createdAt":state::now()});db::put(c,"groups",&id,&group)?;for index in 1..=total{let mut shard=spec.clone();for arg in shard["command"].as_array_mut().unwrap(){*arg=json!(arg.as_str().unwrap().replace("{shard}",&index.to_string()).replace("{total}",&total.to_string()));}if let Some(name)=spec["name"].as_str(){shard["name"]=json!(format!("{}{}",name.replace("{shard}",&index.to_string()).replace("{total}",&total.to_string()),if name.contains("{shard}"){String::new()}else{format!(" [{index}/{total}]")}));}shard["env"]["GRIDSTATOR_SHARD_INDEX"]=json!(index.to_string());shard["env"]["GRIDSTATOR_SHARD_TOTAL"]=json!(total.to_string());create_job(c,shard,&owner,Some((&id,index,total)),None)?;}group_view(c,&id)}).await?;
            let _ = s.events.send(json!({"type":"group.created","group":group}));
            for job in group["jobs"].as_array().unwrap() {
                s.emit_job(job.clone());
            }
            let _ = s.schedule().await;
            return Ok((StatusCode::CREATED, Json(group)).into_response());
        }
        _ => entity_api(&s, &method, &path, &query, input, principal).await?,
    };
    Ok(Json(output).into_response())
}

async fn entity_api(
    s: &Shared,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    input: Value,
    principal: Value,
) -> Result<Value> {
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() >= 3 && parts[0] == "v1",
        "NOT_FOUND: unknown route"
    );
    let kind = parts[1];
    let id = decode_path_component(parts[2])?;
    let action = parts.get(3).copied().unwrap_or("");
    ensure!(parts.len() <= 4, "NOT_FOUND: unknown route");
    let actor = principal["name"].as_str().unwrap_or("client").to_owned();
    match kind {
        "jobs" => {
            let selected = id.clone();
            let job =
                s.db.call(move |c| db::get(c, "jobs", &selected))
                    .await?
                    .context("NOT_FOUND: job")?;
            match (method.as_str(), action) {
                ("GET", "") => Ok(safe_job(job)),
                ("GET", "events") => {
                    let events =
                        s.db.call(move |c| {
                            Ok(db::list(c, "events")?
                                .into_iter()
                                .filter(|event| event["jobId"] == id)
                                .collect::<Vec<_>>())
                        })
                        .await?;
                    Ok(json!({"events":events}))
                }
                ("GET", "logs") => {
                    let after = query
                        .get("after")
                        .map(|v| v.parse::<i64>())
                        .transpose()?
                        .unwrap_or(0);
                    ensure!(after >= 0, "after must be nonnegative");
                    let limit = query
                        .get("limit")
                        .map(|v| v.parse::<usize>())
                        .transpose()?
                        .unwrap_or(10000);
                    ensure!((1..=10000).contains(&limit), "log limit must be 1..10000");
                    let entries = s.db.call(move |c| db::logs(c, &id, after, limit)).await?;
                    let last = entries
                        .last()
                        .and_then(|entry| entry["s"].as_i64())
                        .unwrap_or(after);
                    Ok(
                        json!({"entries":entries,"complete":protocol::terminal(job["state"].as_str().unwrap())&&last>=job["logSeq"].as_i64().unwrap_or(0)}),
                    )
                }
                ("GET", "artifacts") => Ok(json!({"artifacts":job["artifacts"]})),
                ("POST", "cancel") => s.cancel_job(&id, &actor, "cancelled by client").await,
                ("POST", "retry") => {
                    let new =
                        s.db.write(move |c| {
                            let job = db::get(c, "jobs", &id)?.context("NOT_FOUND: job")?;
                            ensure!(
                                protocol::terminal(job["state"].as_str().unwrap()),
                                "CONFLICT: only terminal jobs can be retried"
                            );
                            let group = job["groupId"].as_str().map(|group| {
                                (
                                    group,
                                    job["shardIndex"].as_u64().unwrap_or(1),
                                    job["shardTotal"].as_u64().unwrap_or(1),
                                )
                            });
                            let retry =
                                create_job(c, job["spec"].clone(), &actor, group, Some(&job))?;
                            record_event(
                                c,
                                &job,
                                "retried",
                                &actor,
                                None,
                                None,
                                Some(&format!("retried as {}", retry["id"].as_str().unwrap())),
                            )?;
                            Ok(retry)
                        })
                        .await?;
                    s.emit_job(new.clone());
                    s.schedule().await?;
                    Ok(safe_job(new))
                }
                _ => anyhow::bail!("NOT_FOUND: job action"),
            }
        }
        "nodes" => {
            ensure!(action.is_empty(), "NOT_FOUND: node action");
            let selected = id.clone();
            let online = s.online();
            let node =
                s.db.call(move |c| {
                    let node = db::list(c, "nodes")?
                        .into_iter()
                        .find(|node| node["id"] == selected || node["name"] == selected)
                        .context("NOT_FOUND: node")?;
                    let connected = online.contains(node["id"].as_str().unwrap());
                    node_view(c, node, connected)
                })
                .await?;
            let node_id = node["id"].as_str().unwrap().to_owned();
            match method.as_str() {
                "GET" => Ok(node),
                "PATCH" => {
                    let selected = node_id.clone();
                    // Keep durable mutations and their control frames in the same order when
                    // clients rename a worker while another request changes its settings.
                    let coordination = s.lifecycle.lock().await;
                    let online = s.online();
                    let (updated, renamed) =
                        s.db.write(move |c| {
                            let mut node =
                                db::get(c, "nodes", &selected)?.context("NOT_FOUND: node")?;
                            let new_name = input
                                .get("name")
                                .map(|name| {
                                    name.as_str()
                                        .context("BAD_REQUEST: Node name must be a string")
                                })
                                .transpose()?;
                            if let Some(name) = new_name {
                                validate_node_name(name)?;
                                ensure!(
                                    !db::list(c, "nodes")?
                                        .iter()
                                        .any(|other| other["id"] != selected
                                            && (other["name"] == name || other["id"] == name)),
                                    "CONFLICT: node name already exists or conflicts with a node ID"
                                );
                            }
                            node["settings"] =
                                merge_node_settings(node["settings"].clone(), &input)?;
                            let renamed = new_name.is_some_and(|name| node["name"] != name);
                            if let Some(name) = new_name.filter(|_| renamed) {
                                rename_node_references(
                                    c,
                                    node["name"].as_str().context("node name required")?,
                                    &selected,
                                )?;
                                node["name"] = json!(name);
                            }
                            db::put(c, "nodes", &selected, &node)?;
                            Ok((node_view(c, node, online.contains(&selected))?, renamed))
                        })
                        .await?;
                    let mut message =
                        json!({"type":"node.settings","settings":updated["settings"]});
                    if renamed {
                        message["name"] = updated["name"].clone();
                    }
                    s.send_agent(&node_id, message);
                    let _ = s.events.send(json!({"type":"node.updated","node":updated}));
                    drop(coordination);
                    s.schedule().await?;
                    Ok(updated)
                }
                "DELETE" => {
                    ensure!(principal["role"] == "admin", "FORBIDDEN: admin required");
                    let force = query
                        .get("force")
                        .is_some_and(|force| force == "true" || force == "1");
                    let selected = node_id.clone();
                    let _coordination = s.lifecycle.lock().await;
                    let jobs =
                        s.db.write(move |c| {
                            let active = db::list(c, "jobs")?
                                .into_iter()
                                .filter(|job| job["nodeId"] == selected && dispatched(job))
                                .collect::<Vec<_>>();
                            ensure!(
                                force || active.is_empty(),
                                "CONFLICT: node still has active jobs; drain it or use --force"
                            );
                            let mut changed = vec![];
                            for mut job in active {
                                job["error"] = Value::Null;
                                changed.extend(finish_job(
                                    c,
                                    &mut job,
                                    "cancelled",
                                    &actor,
                                    Some("node was removed"),
                                )?);
                            }
                            for mut token in db::list(c, "tokens")? {
                                if token["nodeId"] == selected {
                                    token["revokedAt"] = json!(state::now());
                                    db::put(c, "tokens", token["id"].as_str().unwrap(), &token)?;
                                }
                            }
                            db::delete(c, "nodes", &selected)?;
                            Ok(changed)
                        })
                        .await?;
                    s.close_agent(&node_id, None, 4003, "node removed", Some("NODE_REMOVED"));
                    s.disconnected.lock().unwrap().remove(&node_id);
                    s.emit_jobs(jobs);
                    let _ = s
                        .events
                        .send(json!({"type":"node.removed","nodeId":node_id}));
                    Ok(json!({"ok":true}))
                }
                _ => anyhow::bail!("NOT_FOUND: node action"),
            }
        }
        "tokens" => {
            ensure!(principal["role"] == "admin", "FORBIDDEN: admin required");
            ensure!(
                method == Method::DELETE && action.is_empty(),
                "NOT_FOUND: token action"
            );
            let selected = id.clone();
            s.db.write(move |c| {
                let mut token = db::get(c, "tokens", &selected)?.context("NOT_FOUND: token")?;
                token["revokedAt"] = json!(state::now());
                db::put(c, "tokens", &selected, &token)
            })
            .await?;
            s.close_token(&id);
            Ok(json!({"ok":true}))
        }
        "snapshots" => {
            ensure!(
                method == Method::GET && action.is_empty(),
                "NOT_FOUND: snapshot action"
            );
            let manifest = query.get("manifest").is_some_and(|value| value == "true");
            Ok(snapshot_view(
                s.db.call(move |c| db::get(c, "snapshots", &id))
                    .await?
                    .context("NOT_FOUND: snapshot")?,
                manifest,
            ))
        }
        "groups" => {
            let selected = id.clone();
            let group = s.db.call(move |c| group_view(c, &selected)).await?;
            match (method.as_str(), action) {
                ("GET", "") => Ok(group),
                ("POST", "cancel") => {
                    for job in group["jobs"].as_array().unwrap() {
                        if !protocol::terminal(job["state"].as_str().unwrap()) {
                            s.cancel_job(
                                job["id"].as_str().unwrap(),
                                &actor,
                                "group cancelled by client",
                            )
                            .await?;
                        }
                    }
                    s.db.call(move |c| group_view(c, &id)).await
                }
                ("POST", "retry") => {
                    let jobs =
                        s.db.write(move |c| {
                            let group = group_view(c, &id)?;
                            let jobs = group["jobs"].as_array().unwrap();
                            ensure!(
                                jobs.iter()
                                    .all(|job| protocol::terminal(job["state"].as_str().unwrap())),
                                "CONFLICT: group is still running"
                            );
                            ensure!(
                                jobs.iter().any(|job| job["state"] != "succeeded"),
                                "CONFLICT: all shards succeeded"
                            );
                            let mut created = vec![];
                            for view in jobs {
                                if view["state"] == "succeeded" {
                                    continue;
                                }
                                let job = db::get(c, "jobs", view["id"].as_str().unwrap())?
                                    .context("NOT_FOUND: shard")?;
                                let retry = create_job(
                                    c,
                                    job["spec"].clone(),
                                    &actor,
                                    Some((
                                        &id,
                                        job["shardIndex"].as_u64().unwrap(),
                                        job["shardTotal"].as_u64().unwrap(),
                                    )),
                                    Some(&job),
                                )?;
                                record_event(
                                    c,
                                    &job,
                                    "retried",
                                    &actor,
                                    None,
                                    None,
                                    Some("group retry"),
                                )?;
                                created.push(retry);
                            }
                            Ok(created)
                        })
                        .await?;
                    s.emit_jobs(jobs);
                    s.schedule().await?;
                    let selected = group["id"].as_str().unwrap().to_owned();
                    s.db.call(move |c| group_view(c, &selected)).await
                }
                _ => anyhow::bail!("NOT_FOUND: group action"),
            }
        }
        _ => anyhow::bail!("NOT_FOUND: unknown route"),
    }
}

async fn blob_api(s: &Shared, path: &str, body: &Bytes) -> Result<Response> {
    if path == "/v1/blobs/download" {
        let input: Value = serde_json::from_slice(body)?;
        let digests = input["digests"]
            .as_array()
            .context("digests required")?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).context("invalid digest"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(digests.len() <= 100000, "too many digests");
        let store = crate::workspace::BlobStore::new(s.dir.join("blobs"));
        for digest in &digests {
            ensure!(store.has(digest)?, "NOT_FOUND: missing blob");
        }
        let permit = s.io_permits.clone().acquire_owned().await?;
        let (writer, reader) = tokio::io::duplex(64 * 1024);
        let writer = tokio_util::io::SyncIoBridge::new(writer);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if let Err(error) = crate::workspace::pack_blobs(&store, &digests, writer) {
                eprintln!("blob download interrupted: {error}");
            }
        });
        return Ok(Response::builder()
            .header("content-type", "application/x-tar")
            .body(Body::from_stream(tokio_util::io::ReaderStream::new(reader)))?);
    }
    let path = path.to_owned();
    let body = body.to_vec();
    let root = s.dir.join("blobs");
    let instance = s._instance.clone();
    let (result, tar) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, bool)> {
        let _instance = instance;
        let store = crate::workspace::BlobStore::new(root);
        store.init()?;
        if path == "/v1/blobs/upload" {
            let (files, _, bytes) = crate::workspace::unpack_blobs(
                body.as_slice(),
                &store,
                crate::workspace::UnpackLimits {
                    max_entries: Some(100000),
                    max_bytes: Some(64 * 1024 * 1024),
                },
            )?;
            return Ok((
                serde_json::to_vec(&json!({"stored":files,"bytes":bytes}))?,
                false,
            ));
        }
        let input: Value = serde_json::from_slice(&body)?;
        let digests = input["digests"]
            .as_array()
            .context("digests required")?
            .iter()
            .map(|d| d.as_str().map(str::to_owned).context("invalid digest"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(digests.len() <= 100000, "too many digests");
        if path == "/v1/blobs/missing" {
            return Ok((
                serde_json::to_vec(&json!({"missing":store.missing(&digests)?}))?,
                false,
            ));
        }
        ensure!(path == "/v1/blobs/download", "NOT_FOUND: blob route");
        let mut bytes = vec![];
        crate::workspace::pack_blobs(&store, &digests, &mut bytes)?;
        Ok((bytes, true))
    })
    .await??;
    Ok(Response::builder()
        .header(
            "content-type",
            if tar {
                "application/x-tar"
            } else {
                "application/json"
            },
        )
        .body(Body::from(result))?)
}
async fn blob_upload(
    State(s): State<Shared>,
    headers: HeaderMap,
    body: Body,
) -> std::result::Result<Response, ApiError> {
    async {
        s.principal(&headers, &["client", "agent"], false).await?;
        let permit = s.io_permits.clone().acquire_owned().await?;
        let store = crate::workspace::BlobStore::new(s.dir.join("blobs"));
        let stream = body.into_data_stream().map_err(std::io::Error::other);
        let reader = tokio_util::io::SyncIoBridge::new(tokio_util::io::StreamReader::new(stream));
        let instance = s._instance.clone();
        let (stored, present, bytes) = tokio::task::spawn_blocking(move || {
            let _instance = instance;
            let _permit = permit;
            store.init()?;
            crate::workspace::unpack_blobs(
                reader,
                &store,
                crate::workspace::UnpackLimits {
                    max_entries: Some(1000000),
                    max_bytes: Some(8 * 1024 * 1024 * 1024),
                },
            )
        })
        .await??;
        Ok::<_, anyhow::Error>(
            Json(json!({"stored":stored,"present":present,"bytes":bytes})).into_response(),
        )
    }
    .await
    .map_err(ApiError)
}

async fn controller_publish(
    State(s): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
    body: Body,
) -> std::result::Result<Response, ApiError> {
    async {
        use tokio::io::AsyncWriteExt;
        let principal = s.principal(&headers, &["client"], true).await?;
        let query = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect::<HashMap<String, String>>();
        let expected = query
            .get("sha256")
            .context("BAD_REQUEST: bundle sha256 required")?;
        ensure!(
            expected.len() == 64
                && expected
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "BAD_REQUEST: invalid bundle checksum"
        );
        let _permit = s.io_permits.clone().acquire_owned().await?;
        state::private_dir(&s.dir.join("updates"))?;
        let temporary = tempfile::NamedTempFile::new_in(s.dir.join("updates"))?;
        let mut file = tokio::fs::File::from_std(temporary.reopen()?);
        let mut stream = body.into_data_stream();
        let mut size = 0u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            ensure!(
                size <= 512 * 1024 * 1024,
                "BAD_REQUEST: native release bundle exceeds 512 MiB"
            );
            hash.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        drop(file);
        let actual = hex::encode(hash.finalize());
        ensure!(
            actual == *expected,
            "BAD_REQUEST: native bundle checksum mismatch: expected {expected}, got {actual}"
        );
        crate::updates::publish_path(s, temporary.path(), expected, principal).await
    }
    .await
    .map_err(ApiError)
}
async fn agent_upgrade(
    State(s): State<Shared>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, ApiError> {
    let principal = s.principal(&headers, &["agent"], false).await?;
    let node = principal["nodeId"]
        .as_str()
        .context("UNAUTHORIZED: no node identity")?
        .to_owned();
    let token = principal["id"]
        .as_str()
        .context("UNAUTHORIZED: no credential identity")?
        .to_owned();
    Ok(ws
        .max_message_size(16 * 1024 * 1024)
        .on_upgrade(move |socket| agent_socket(s, node, token, socket)))
}
async fn agent_socket(s: Shared, node: String, token: String, socket: WebSocket) {
    let (sender, mut receiver) = mpsc::channel::<Value>(512);
    let generation = state::id();
    let (close, mut closing) = watch::channel::<Option<CloseNotice>>(None);
    let (mut outgoing, mut incoming) = socket.split();
    let (finish, mut finished) = tokio::sync::oneshot::channel::<CloseNotice>();
    let mut writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                value=receiver.recv()=>{let Some(value)=value else{break;};if outgoing.send(Message::Text(value.to_string().into())).await.is_err(){return;}},
                notice=&mut finished=>{receiver.close();while let Ok(value)=receiver.try_recv(){if outgoing.send(Message::Text(value.to_string().into())).await.is_err(){return;}}if let Ok(notice)=notice{let _=outgoing.send(Message::Close(Some(CloseFrame{code:notice.code,reason:notice.reason.into()}))).await;}return;}
            }
        }
    });
    let mut welcomed = false;
    let mut shutdown = s.shutdown.subscribe();
    let handshake = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(handshake);
    let mut notice = CloseNotice {
        code: 1000,
        reason: "connection closed".into(),
    };
    loop {
        let frame = tokio::select! {
            frame=incoming.next()=>frame,
            _=closing.changed()=>{notice=closing.borrow().clone().unwrap_or(CloseNotice{code:1001,reason:"connection replaced".into()});break;},
            _=shutdown.changed()=>{notice=CloseNotice{code:1001,reason:"controller shutting down".into()};break;},
            _=&mut handshake,if !welcomed=>{notice=CloseNotice{code:4000,reason:"hello timeout".into()};break;}
        };
        let Some(Ok(frame)) = frame else {
            break;
        };
        let Message::Text(raw) = frame else {
            if matches!(frame, Message::Close(_)) {
                break;
            }
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(&raw) else {
            notice = CloseNotice {
                code: 4002,
                reason: "invalid JSON".into(),
            };
            break;
        };
        if !welcomed && message["type"] != "hello" {
            notice = CloseNotice {
                code: 4002,
                reason: "hello required".into(),
            };
            break;
        }
        if message["type"] == "hello" {
            if message["protocolVersion"] != 1 || validate_node_info(&message["info"]).is_err() {
                let _=sender.send(json!({"type":"error","code":"PROTOCOL_MISMATCH","message":"native v2 agent and protocol version 1 are required","fatal":true})).await;
                notice = CloseNotice {
                    code: 4002,
                    reason: "protocol mismatch".into(),
                };
                break;
            }
            let _coordination = s.lifecycle.lock().await;
            if welcomed
                && !s
                    .agents
                    .lock()
                    .unwrap()
                    .get(&node)
                    .is_some_and(|connection| connection.generation == generation)
            {
                break;
            }
            let id = node.clone();
            let token_id = token.clone();
            let hello = message.clone();
            let already_welcomed = welcomed;
            let updated =
                s.db.write(move |c| {
                    ensure!(
                        valid_credential(c, &token_id, "agent", Some(&id), state::now())?,
                        "UNAUTHORIZED: agent credential revoked"
                    );
                    let active = hello["activeJobs"]
                        .as_array()
                        .context("hello activeJobs required")?;
                    let results = hello["results"]
                        .as_array()
                        .context("hello results required")?;
                    ensure!(
                        active.len() <= 256 && results.len() <= 10000,
                        "too many hello jobs"
                    );
                    for report in active {
                        ensure!(
                            report["jobId"].is_string()
                                && report["assignmentId"].is_string()
                                && matches!(report["phase"].as_str(), Some("starting" | "running"))
                                && report["logSeq"].as_u64().is_some(),
                            "invalid active job report"
                        );
                    }
                    for result in results {
                        protocol::normalize_result(result.clone())?;
                    }
                    let mut n = db::get(c, "nodes", &id)?.context("UNAUTHORIZED: removed node")?;
                    n["info"] = hello["info"].clone();
                    n["settings"] = merge_node_settings(n["settings"].clone(), &hello["settings"])?;
                    if !already_welcomed {
                        n["connectedAt"] = json!(state::now());
                    }
                    n["lastSeenAt"] = json!(state::now());
                    db::put(c, "nodes", &id, &n)?;
                    let mut resume = vec![];
                    let mut changed = vec![];
                    let mut cancels = vec![];
                    let mut acks = vec![];
                    for mut job in db::list(c, "jobs")? {
                        if job["nodeId"] != id || !dispatched(&job) {
                            continue;
                        }
                        let running = active.iter().find(|report| {
                            report["jobId"] == job["id"]
                                && report["assignmentId"] == job["assignmentId"]
                        });
                        let pending = results.iter().any(|report| {
                            report["jobId"] == job["id"]
                                && report["assignmentId"] == job["assignmentId"]
                        });
                        if let Some(report) = running {
                            if let Some(phase) = report["phase"].as_str()
                                && ((job["state"] == "assigned"
                                    && matches!(phase, "starting" | "running"))
                                    || (job["state"] == "starting" && phase == "running"))
                            {
                                transition_as(
                                    c,
                                    &mut job,
                                    phase,
                                    None,
                                    &format!("node:{}", n["name"].as_str().unwrap_or("unknown")),
                                )?;
                                changed.push(job.clone());
                            }
                            resume.push(json!({"jobId":job["id"],"logSeq":job["logSeq"]}));
                            if !job["cancelRequestedAt"].is_null() {
                                cancels.push(job["id"].clone());
                            }
                        } else if pending {
                            resume.push(json!({"jobId":job["id"],"logSeq":job["logSeq"]}));
                        } else if already_welcomed {
                            // Later hellos refresh capabilities/admission on this connection.
                            // Their inventory can predate an in-flight assignment or result;
                            // absence is authoritative only on a new connection's first hello.
                            continue;
                        } else if !job["cancelRequestedAt"].is_null() {
                            job["error"] = Value::Null;
                            changed.extend(finish_job(
                                c,
                                &mut job,
                                "cancelled",
                                "controller",
                                Some("cancelled job absent on reconnect"),
                            )?);
                        } else if job["state"] == "assigned" {
                            requeue(c, &mut job, "assignment absent on reconnect", "controller")?;
                            changed.push(job);
                        } else {
                            job["error"] = json!(format!(
                                "The agent on {} restarted or lost track of the job",
                                n["name"].as_str().unwrap_or("unknown")
                            ));
                            changed.extend(finish_job(
                                c,
                                &mut job,
                                "lost",
                                "controller",
                                Some("agent restarted without running job"),
                            )?);
                        }
                    }
                    for report in active {
                        let valid = db::get(c, "jobs", report["jobId"].as_str().unwrap())?
                            .is_some_and(|job| {
                                job["nodeId"] == id
                                    && job["assignmentId"] == report["assignmentId"]
                                    && dispatched(&job)
                            });
                        if !valid {
                            cancels.push(report["jobId"].clone());
                        }
                    }
                    for report in results {
                        let valid = db::get(c, "jobs", report["jobId"].as_str().unwrap())?
                            .is_some_and(|job| {
                                job["nodeId"] == id
                                    && job["assignmentId"] == report["assignmentId"]
                                    && dispatched(&job)
                            });
                        if !valid {
                            acks.push(report["jobId"].clone());
                        }
                    }
                    Ok((n, resume, changed, cancels, acks))
                })
                .await;
            let Ok((n, resume, jobs, cancels, acks)) = updated else {
                notice = CloseNotice {
                    code: 4001,
                    reason: "invalid or revoked agent hello".into(),
                };
                break;
            };
            if !welcomed {
                s.close_agent(&node, None, 4000, "replaced by a newer connection", None);
            }
            s.agents.lock().unwrap().insert(
                node.clone(),
                AgentConnection {
                    sender: sender.clone(),
                    generation: generation.clone(),
                    token_id: token.clone(),
                    last_message_at: state::now(),
                    close: close.clone(),
                },
            );
            s.disconnected.lock().unwrap().remove(&node);
            let online = s.online();
            let public =
                s.db.write(move |c| {
                    let public = node_view(c, n, true)?;
                    refresh_nodes(c, &online)?;
                    Ok(public)
                })
                .await;
            let mut welcome = json!({"type":"welcome","clusterId":s.config["clusterId"],"nodeId":node,"name":public.as_ref().map(|n|n["name"].clone()).unwrap_or(Value::Null),"settings":public.as_ref().map(|n|n["settings"].clone()).unwrap_or(Value::Null),"resume":resume});
            if let Some(url) = s.config["publicUrl"].as_str() {
                welcome["controllerUrl"] = json!(url);
            }
            let _ = sender.try_send(welcome);
            for id in cancels {
                let _=sender.try_send(json!({"type":"job.cancel","jobId":id,"reason":"job no longer active or cancellation pending"}));
            }
            for id in acks {
                let _ = sender.try_send(json!({"type":"job.ack","jobId":id}));
            }
            s.emit_jobs(jobs);
            if let Ok(node) = public {
                let _ = s.events.send(json!({"type":"node.updated","node":node}));
            }
            welcomed = true;
            drop(_coordination);
            let _ = s.schedule().await;
        } else {
            match apply_agent_message(&s, &node, &generation, message).await {
                Ok(Some(ack)) => {
                    let _ = sender.try_send(ack);
                }
                Ok(None) => {}
                Err(error) => eprintln!("agent message rejected: {error}"),
            }
            let _ = s.schedule().await;
        }
    }
    {
        let _coordination = s.lifecycle.lock().await;
        if s.close_agent(&node, Some(&generation), notice.code, &notice.reason, None) {
            let online = s.online();
            if let Ok(nodes) = s.db.write(move |c| refresh_nodes(c, &online)).await {
                for node in nodes {
                    let _ = s.events.send(json!({"type":"node.updated","node":node}));
                }
            }
        }
    }
    let _ = finish.send(notice);
    drop(sender);
    // Flush fatal errors before the Close frame, including a failed protocol negotiation.
    if tokio::time::timeout(Duration::from_secs(1), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}
async fn apply_agent_message(
    s: &Shared,
    node: &str,
    generation: &str,
    mut message: Value,
) -> Result<Option<Value>> {
    let _coordination = s.lifecycle.lock().await;
    let connection = s
        .agents
        .lock()
        .unwrap()
        .get(node)
        .filter(|connection| connection.generation == generation)
        .cloned();
    let Some(connection) = connection else {
        return Ok(None);
    };
    let id = node.to_owned();
    let kind = message["type"]
        .as_str()
        .context("message type required")?
        .to_owned();
    if kind == "job.result" {
        message["result"] = protocol::normalize_result(message["result"].clone())?;
    }
    if kind == "job.outputs" {
        message["outputs"] = protocol::normalize_outputs(message["outputs"].clone())?;
    }
    let token = connection.token_id;
    let online = s.online();
    let rejection_ttl = s.timing.rejection_ttl_ms;
    let (jobs, ack, logs, cancels, nodes) =
        s.db.write(move |c| {
            ensure!(
                valid_credential(c, &token, "agent", Some(&id), state::now())?,
                "UNAUTHORIZED: agent credential revoked"
            );
            let mut n = db::get(c, "nodes", &id)?.context("UNAUTHORIZED: node removed")?;
            n["lastSeenAt"] = json!(state::now());
            if kind == "heartbeat" {
                validate_metrics(&message["metrics"])?;
                n["metrics"] = message["metrics"].clone();
                db::put(c, "nodes", &id, &n)?;
                let active = message["activeJobs"]
                    .as_array()
                    .context("heartbeat activeJobs required")?;
                ensure!(active.len() <= 256, "too many active jobs");
                let mut cancels = vec![];
                for job_id in active {
                    let job_id = job_id.as_str().context("invalid active job ID")?;
                    if db::get(c, "jobs", job_id)?
                        .is_none_or(|job| job["nodeId"] != id || !dispatched(&job))
                    {
                        cancels.push(job_id.to_owned());
                    }
                }
                return Ok((vec![], None, None, cancels, refresh_nodes(c, &online)?));
            }
            db::put(c, "nodes", &id, &n)?;
            let result = if kind == "job.result" {
                &message["result"]
            } else {
                &message
            };
            let job_id = result["jobId"]
                .as_str()
                .context("jobId required")?
                .to_owned();
            let ack = if kind == "job.result" {
                Some(json!({"type":"job.ack","jobId":job_id}))
            } else {
                None
            };
            let Some(mut job) = db::get(c, "jobs", &job_id)? else {
                return Ok((vec![], ack, None, vec![], vec![]));
            };
            // Acknowledge obsolete durable results so old outboxes can quiesce after reassignment/GC.
            if job["nodeId"] != id
                || job["assignmentId"] != result["assignmentId"]
                || !dispatched(&job)
            {
                return Ok((vec![], ack, None, vec![], vec![]));
            }
            let mut changed = vec![];
            let mut logs = None;
            let actor = format!("node:{}", job["nodeName"].as_str().unwrap_or("unknown"));
            match kind.as_str() {
                "job.state" => {
                    let phase = message["phase"].as_str().context("phase required")?;
                    ensure!(
                        matches!(phase, "starting" | "running") && message["ts"].as_u64().is_some(),
                        "invalid job phase/timestamp"
                    );
                    if (job["state"] == "assigned" && matches!(phase, "starting" | "running"))
                        || (job["state"] == "starting" && phase == "running")
                    {
                        if phase == "running" {
                            job["startedAt"] = message["ts"].clone();
                        }
                        transition_as(c, &mut job, phase, None, &actor)?;
                        changed.push(job);
                    }
                }
                "job.log" => {
                    let entries = message["entries"]
                        .as_array()
                        .context("log entries required")?;
                    ensure!(entries.len() <= 10000, "too many log entries");
                    let mut accepted = vec![];
                    for entry in entries {
                        let seq = entry["s"]
                            .as_i64()
                            .filter(|n| *n > 0)
                            .context("invalid log sequence")?;
                        ensure!(
                            entry["t"].as_u64().is_some()
                                && matches!(
                                    entry["o"].as_str(),
                                    Some("stdout" | "stderr" | "system")
                                )
                                && entry["d"].as_str().is_some_and(|s| s.len() <= 1024 * 1024),
                            "invalid log entry"
                        );
                        if seq <= job["logSeq"].as_i64().unwrap_or(0) {
                            continue;
                        }
                        ensure!(
                            seq == job["logSeq"].as_i64().unwrap_or(0) + 1,
                            "log sequence gap"
                        );
                        let normalized =
                            json!({"s":seq,"t":entry["t"],"o":entry["o"],"d":entry["d"]});
                        c.execute(
                            "INSERT OR IGNORE INTO logs(job_id,seq,data)VALUES(?1,?2,?3)",
                            rusqlite::params![job_id, seq, normalized.to_string()],
                        )?;
                        job["logSeq"] = json!(seq);
                        accepted.push(normalized);
                    }
                    db::put(c, "jobs", &job_id, &job)?;
                    logs = Some(json!({"type":"logs","jobId":job_id,"entries":accepted}));
                }
                "job.outputs" => {
                    job["outputs"] = message["outputs"].clone();
                    db::put(c, "jobs", &job_id, &job)?;
                    changed.push(job);
                }
                "job.result" => {
                    ensure!(
                        result["logSeq"].as_i64().unwrap_or(0)
                            <= job["logSeq"].as_i64().unwrap_or(0),
                        "result arrived before durable logs"
                    );
                    let outcome = if !job["cancelRequestedAt"].is_null()
                        && result["outcome"] != "succeeded"
                    {
                        "cancelled"
                    } else {
                        result["outcome"].as_str().unwrap()
                    };
                    job["exitCode"] = result["exitCode"].clone();
                    job["error"] = if outcome == "cancelled" {
                        Value::Null
                    } else {
                        result["error"].clone()
                    };
                    job["oomKilled"] = result["oomKilled"].clone();
                    if !result["outputs"].is_null() {
                        job["outputs"] = result["outputs"].clone();
                    }
                    if result["artifacts"]
                        .as_array()
                        .is_some_and(|artifacts| !artifacts.is_empty())
                    {
                        job["artifacts"] = result["artifacts"].clone();
                    }
                    job["finishedAt"] = result["finishedAt"].clone();
                    changed.extend(finish_job(
                        c,
                        &mut job,
                        outcome,
                        &actor,
                        result["error"].as_str(),
                    )?);
                }
                "job.rejected" => {
                    let reason = message["reason"]
                        .as_str()
                        .context("rejection reason required")?;
                    ensure!(
                        reason.encode_utf16().count() <= 4096,
                        "rejection reason too long"
                    );
                    if job["state"] == "assigned" {
                        let mut exclusions = db::get(c, "rejections", &job_id)?
                            .unwrap_or(json!({"jobId":job_id,"nodes":{}}));
                        // Native update probes briefly close admission while checking an exact
                        // build/tool target. An assignment already in flight needs a short retry,
                        // rather than the source's long exclusion for an unusable executor.
                        let retry_delay = if matches!(
                            reason,
                            "agent is checking an update"
                                | "agent is draining or checking an update"
                        ) {
                            rejection_ttl.min(1_000)
                        } else {
                            rejection_ttl
                        };
                        exclusions["nodes"][&id] = json!(state::now() + retry_delay);
                        db::put(c, "rejections", &job_id, &exclusions)?;
                        requeue(c, &mut job, reason, &actor)?;
                        changed.push(job);
                    } else if job["state"] == "starting" {
                        record_event(
                            c,
                            &job,
                            "rejection-ignored",
                            &actor,
                            None,
                            None,
                            Some("invalid starting to queued transition; retained source behavior"),
                        )?;
                    } else {
                        job["error"] = json!(reason);
                        changed.extend(finish_job(c, &mut job, "failed", &actor, Some(reason))?);
                    }
                }
                _ => anyhow::bail!("unsupported agent message"),
            }
            Ok((changed, ack, logs, vec![], refresh_nodes(c, &online)?))
        })
        .await?;
    if let Some(current) = s
        .agents
        .lock()
        .unwrap()
        .get_mut(node)
        .filter(|current| current.generation == generation)
    {
        current.last_message_at = state::now();
    }
    for job in cancels {
        s.send_agent(
            node,
            json!({"type":"job.cancel","jobId":job,"reason":"job is no longer active"}),
        );
    }
    if let Some(logs) = logs {
        let _ = s.events.send(logs);
    }
    s.emit_jobs(jobs);
    for node in nodes {
        let _ = s.events.send(json!({"type":"node.updated","node":node}));
    }
    Ok(ack)
}

struct ClientSession {
    controller: Shared,
    id: String,
}
impl Drop for ClientSession {
    fn drop(&mut self) {
        self.controller
            .client_sessions
            .lock()
            .unwrap()
            .remove(&self.id);
    }
}
async fn events_upgrade(
    State(s): State<Shared>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, ApiError> {
    let principal = s.principal(&headers, &["client"], false).await?;
    let token = principal["id"].as_str().unwrap().to_owned();
    Ok(ws
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| events_socket(s, token, socket)))
}
async fn send_event(socket: &mut WebSocket, message: Value) -> bool {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .is_ok()
}
/// Page a historical log subscription while live messages accumulate in the broadcast receiver.
async fn send_log_backlog(
    s: &Shared,
    socket: &mut WebSocket,
    subscribed: &mut HashMap<String, i64>,
    id: String,
    mut after: i64,
) -> bool {
    subscribed.insert(id.clone(), after);
    loop {
        let selected = id.clone();
        let Ok((entries, job)) =
            s.db.call(move |c| {
                Ok((
                    db::logs(c, &selected, after, 128)?,
                    db::get(c, "jobs", &selected)?,
                ))
            })
            .await
        else {
            return true;
        };
        let count = entries.len();
        after = entries
            .last()
            .and_then(|entry| entry["s"].as_i64())
            .unwrap_or(after);
        subscribed.insert(id.clone(), after);
        if !send_event(socket, json!({"type":"logs","jobId":id,"entries":entries})).await {
            return false;
        }
        if count < 128 {
            if let Some(job) = job
                && protocol::terminal(job["state"].as_str().unwrap_or(""))
                && after >= job["logSeq"].as_i64().unwrap_or(0)
            {
                if !send_event(
                    socket,
                    json!({"type":"logs.end","jobId":id,"state":job["state"]}),
                )
                .await
                {
                    return false;
                }
                subscribed.remove(&id);
            }
            return true;
        }
    }
}
fn client_event_messages(
    event: Value,
    topics: &HashSet<String>,
    subscribed: &mut HashMap<String, i64>,
) -> Vec<Value> {
    let kind = event["type"].as_str().unwrap_or("");
    let mut messages = vec![];
    if kind == "logs" {
        let id = event["jobId"].as_str().unwrap_or("");
        if let Some(after) = subscribed.get_mut(id) {
            let entries = event["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| entry["s"].as_i64().unwrap_or(0) > *after)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(last) = entries.last() {
                *after = last["s"].as_i64().unwrap();
                messages.push(json!({"type":"logs","jobId":id,"entries":entries}));
            }
        }
    } else if kind == "job.updated" {
        if topics.contains("jobs") {
            messages.push(event.clone());
        }
        let id = event["job"]["id"].as_str().unwrap_or("");
        if let Some(after) = subscribed.get(id)
            && protocol::terminal(event["job"]["state"].as_str().unwrap_or(""))
            && *after >= event["job"]["logSeq"].as_i64().unwrap_or(0)
        {
            messages.push(json!({"type":"logs.end","jobId":id,"state":event["job"]["state"]}));
            subscribed.remove(id);
        }
    } else if (kind.starts_with("node.") && topics.contains("nodes"))
        || (kind.starts_with("group.") && topics.contains("jobs"))
    {
        messages.push(event);
    }
    messages
}
async fn events_socket(s: Shared, token: String, mut socket: WebSocket) {
    let session = state::id();
    let (close, mut closing) = watch::channel(false);
    s.client_sessions
        .lock()
        .unwrap()
        .insert(session.clone(), (token.clone(), close));
    let _session = ClientSession {
        controller: s.clone(),
        id: session,
    };
    let mut shutdown = s.shutdown.subscribe();
    let mut receiver = s.events.subscribe();
    let mut topics = HashSet::<String>::new();
    let mut subscribed = HashMap::<String, i64>::new();
    let mut authorization = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _=closing.changed()=>{let _=socket.send(Message::Close(Some(CloseFrame{code:4001,reason:"credential revoked".into()}))).await;break;},
            _=shutdown.changed()=>{let _=socket.send(Message::Close(Some(CloseFrame{code:1001,reason:"controller shutting down".into()}))).await;break;},
            _=authorization.tick()=>{let token=token.clone();if !s.db.call(move|c|valid_credential(c,&token,"client",None,state::now())).await.unwrap_or(false){let _=socket.send(Message::Close(Some(CloseFrame{code:4001,reason:"credential expired or revoked".into()}))).await;break;}},
            incoming=socket.recv()=>{
                let Some(Ok(frame))=incoming else{break;};let Message::Text(raw)=frame else{if matches!(frame,Message::Close(_)){break;}continue;};let Ok(message)=serde_json::from_str::<Value>(&raw)else{continue;};
                let selected=token.clone();if !s.db.call(move|c|valid_credential(c,&selected,"client",None,state::now())).await.unwrap_or(false){break;}
                match message["type"].as_str(){
                    Some("subscribe")=>{
                        if let Some(values)=message["topics"].as_array().filter(|values|values.iter().all(|value|matches!(value.as_str(),Some("jobs"|"nodes")))) {
                            topics=values.iter().filter_map(|value|value.as_str().map(str::to_owned)).collect();
                        }
                    },
                    Some("logs.unsubscribe")=>{subscribed.remove(message["jobId"].as_str().unwrap_or(""));},
                    Some("logs.subscribe")=>{
                        let Some(id)=message["jobId"].as_str().map(str::to_owned)else{continue;};let after=message["afterSeq"].as_i64().unwrap_or(0).max(0);
                        if !send_log_backlog(&s,&mut socket,&mut subscribed,id,after).await{return;}
                    },
                    _=>{}
                }
            },
            event=receiver.recv()=>{
                let event=match event{Ok(event)=>event,Err(_)=>break};
                for message in client_event_messages(event,&topics,&mut subscribed){if !send_event(&mut socket,message).await{return;}}
            }
        }
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    #[tokio::test]
    async fn shutdown_joins_update_mutation_and_abort_keeps_startup_lease() -> Result<()> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        for abort in [false, true] {
            let temp = tempfile::tempdir()?;
            let dir = temp.path().join("controller");
            initialize(
                &dir,
                "127.0.0.1",
                0,
                &[],
                false,
                &temp.path().join("config.json"),
            )
            .await?;
            let (ready, started) = tokio::sync::oneshot::channel();
            let ready = Arc::new(Mutex::new(Some(ready)));
            let (release, gate) = std::sync::mpsc::channel();
            let gate = Arc::new(Mutex::new(Some(gate)));
            let stop = tokio_util::sync::CancellationToken::new();
            let marker = dir.join("mutation-finished");
            let written = marker.clone();
            let mut server = tokio::spawn(start_controlled_with_updates(
                dir.clone(),
                None,
                None,
                ControllerTiming::default(),
                stop.clone(),
                move |controller, _cancel| {
                    let ready = ready.lock().unwrap().take();
                    let gate = gate.lock().unwrap().take();
                    let written = written.clone();
                    async move {
                        tokio::task::spawn_blocking(move || -> Result<()> {
                            let _owner = controller;
                            let _ = ready.context("updater started twice")?.send(());
                            gate.context("updater started twice")?.recv()?;
                            std::fs::write(written, b"complete")?;
                            Ok(())
                        })
                        .await??;
                        Ok(())
                    }
                },
            ));
            tokio::time::timeout(Duration::from_secs(20), started)
                .await
                .context("waiting for update mutation to start")??;
            if abort {
                server.abort();
                assert!((&mut server).await.unwrap_err().is_cancelled());
            } else {
                stop.cancel();
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut server)
                        .await
                        .is_err(),
                    "controller returned before started mutation finished"
                );
            }
            assert!(!marker.exists());
            let error = Controller::open(dir.clone())
                .await
                .err()
                .context("second controller acquired active mutation data")?;
            assert!(
                error.to_string().contains("another controller owns"),
                "{error:#}"
            );
            release.send(())?;
            if !abort {
                tokio::time::timeout(Duration::from_secs(20), server)
                    .await
                    .context("waiting for controller shutdown after mutation release")???;
            }
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    match Controller::open(dir.clone()).await {
                        Ok(controller) => {
                            assert_eq!(std::fs::read(&marker)?, b"complete");
                            drop(controller);
                            return Ok::<(), anyhow::Error>(());
                        }
                        Err(error) if error.to_string().contains("another controller owns") => {
                            tokio::time::sleep(Duration::from_millis(10)).await
                        }
                        Err(error) => return Err(error),
                    }
                }
            })
            .await
            .context("waiting for the released controller lease")??;
        }
        Ok(())
    }
    #[tokio::test]
    async fn active_controller_lease_blocks_recovery_before_it_reads_switch_marker() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("controller");
        initialize(
            &dir,
            "127.0.0.1",
            0,
            &[],
            false,
            &temp.path().join("config.json"),
        )
        .await?;
        let owner = Controller::open(dir.clone()).await?;
        std::fs::create_dir_all(dir.join("updates"))?;
        let marker = dir.join("updates/controller-switch.json");
        std::fs::write(&marker, b"deliberately invalid recovery marker")?;
        let error = Controller::open(dir)
            .await
            .err()
            .context("second controller acquired owned state")?;
        assert!(
            error.to_string().contains("another controller owns"),
            "recovery ran before startup lease: {error:#}"
        );
        assert_eq!(
            std::fs::read(marker)?,
            b"deliberately invalid recovery marker"
        );
        drop(owner);
        Ok(())
    }
}

#[cfg(test)]
mod api_error_tests {
    use super::*;
    async fn body(response: Response) -> Result<Value> {
        Ok(serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024).await?,
        )?)
    }
    #[tokio::test]
    async fn explicit_http_codes_match_the_recorded_status_table() -> Result<()> {
        let fixture: Value =
            serde_json::from_str(include_str!("../contracts/behavior/errors-and-units.json"))?;
        let errors = fixture["errors"]
            .as_object()
            .context("frozen error table")?;
        assert_eq!(errors.len(), 10);
        for (code, expected) in errors {
            let response = ApiError(anyhow::anyhow!("{code}: frozen status probe")).into_response();
            assert_eq!(
                response.status().as_u16(),
                expected["status"].as_u64().unwrap() as u16,
                "{code}"
            );
            assert_eq!(body(response).await?["error"]["code"], code.as_str());
            let response = ApiError(
                anyhow::anyhow!("{code}: domain cause").context("request operation failed"),
            )
            .into_response();
            assert_eq!(
                response.status().as_u16(),
                expected["status"].as_u64().unwrap() as u16,
                "chained {code}"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn actual_filesystem_and_sqlite_failures_are_internal_server_errors() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("controller");
        initialize(
            &dir,
            "127.0.0.1",
            0,
            &[],
            false,
            &temp.path().join("config.json"),
        )
        .await?;
        let controller = Controller::open(dir.clone()).await?;
        std::fs::remove_file(dir.join("pki/ca.pem"))?;
        let response = ca(State(controller))
            .await
            .err()
            .context("CA handler should fail for a missing internal file")?
            .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let response = body(response).await?;
        assert_eq!(response["error"]["code"], "INTERNAL");
        assert_eq!(
            response["error"]["message"],
            "Internal controller error (see controller logs)"
        );
        let database = rusqlite::Connection::open_in_memory()?;
        let sqlite = database
            .execute_batch("SELECT * FROM missing_internal_table")
            .unwrap_err();
        let response =
            ApiError(anyhow::Error::new(sqlite).context("query controller state")).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body(response).await?["error"]["code"], "INTERNAL");
        Ok(())
    }
    #[tokio::test]
    async fn input_validation_remains_bad_request_and_illegal_transition_is_conflict() -> Result<()>
    {
        let invalid = protocol::normalize_spec(json!({"kind":"command","command":[]})).unwrap_err();
        let response = ApiError(invalid).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body(response).await?["error"]["code"], "BAD_REQUEST");
        let connection = rusqlite::Connection::open_in_memory()?;
        let mut job = json!({"id":"probe","state":"running"});
        let illegal =
            transition_as(&connection, &mut job, "queued", None, "controller").unwrap_err();
        let response = ApiError(illegal).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body(response).await?["error"]["code"], "CONFLICT");
        Ok(())
    }
}
