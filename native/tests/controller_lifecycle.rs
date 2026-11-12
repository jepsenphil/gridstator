use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use gridstator::{
    client::{Client, Config},
    controller::{self, ControllerTiming},
    pki, state,
};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct Cluster {
    temp: TempDir,
    server: tokio::task::JoinHandle<Result<()>>,
    client: Client,
}
impl Drop for Cluster {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn timing() -> ControllerTiming {
    ControllerTiming {
        heartbeat_timeout_ms: 20_000,
        lost_timeout_ms: 120,
        cancel_timeout_ms: 2_000,
        assign_timeout_ms: 20_000,
        startup_grace_ms: 120,
        rejection_ttl_ms: 1_000,
        sweep_interval: Duration::from_millis(20),
        schedule_interval: Duration::from_millis(25),
        retention_interval: Duration::from_secs(3600),
    }
}
async fn boot_with(timing: ControllerTiming) -> Result<Cluster> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = TempDir::new()?;
    let dir = temp.path().join("controller");
    let init = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &temp.path().join("client.json"),
    )
    .await?;
    let server = tokio::spawn(controller::start_with_timing(
        dir.clone(),
        None,
        None,
        timing,
    ));
    let runtime = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(runtime) = state::read_json(&dir.join("runtime.json")) {
                break runtime;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let client = Client::new(Config {
        version: 1,
        url: format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap()),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: init["caFingerprint"].as_str().unwrap().into(),
        token: init["token"].as_str().unwrap().into(),
        token_name: Some("local-admin".into()),
    })?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client.get("/v1/health").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(Cluster {
        temp,
        server,
        client,
    })
}
fn info() -> Value {
    json!({"hostname":"scripted","platform":"darwin","arch":"arm64","osRelease":"test","cpus":16,"memoryTotalMb":32768,"agentVersion":gridstator::VERSION,"docker":{"available":true},"buildx":{"available":false},"act":{"available":false},"executors":["command"],"executionCapacity":{"cpus":4,"memoryTotalMb":4096}})
}
async fn register(c: &Client, name: &str) -> Result<Value> {
    let token = c
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"pairing","maxUses":1}),
        )
        .await?;
    let mut config = c.config.clone();
    config.token = token["token"].as_str().unwrap().into();
    Client::new(config)?.post("/v1/agents/register",json!({"name":name,"info":info(),"settings":{"maxConcurrency":1,"reserved":{"cpus":1,"memoryMb":256}}})).await
}
async fn raw_socket(c: &Client, path: &str, token: &str) -> Result<Socket> {
    let mut request =
        format!("{}{path}", c.config.url.replacen("https:", "wss:", 1)).into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    Ok(tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(pki::websocket_tls(
            &c.config.ca_pem,
        )?)),
    )
    .await?
    .0)
}
async fn send(socket: &mut Socket, value: Value) -> Result<()> {
    socket.send(Message::Text(value.to_string().into())).await?;
    Ok(())
}
async fn receive(socket: &mut Socket, kind: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = socket.next().await.context("socket ended")??;
            if frame.is_close() {
                bail!("socket closed: {frame:?}");
            }
            if let Ok(text) = frame.to_text() {
                let value: Value = serde_json::from_str(text)?;
                if value["type"] == kind {
                    return Ok(value);
                }
            }
        }
    })
    .await
    .with_context(|| format!("waiting for socket message {kind}"))?
}
async fn connect(c: &Client, agent: &Value, active: Value, results: Value) -> Result<Socket> {
    let mut socket = raw_socket(c, "/v1/agent/ws", agent["credential"].as_str().unwrap()).await?;
    send(&mut socket,json!({"type":"hello","protocolVersion":1,"info":info(),"activeJobs":active,"results":results})).await?;
    receive(&mut socket, "welcome").await?;
    Ok(socket)
}
async fn submit(c: &Client, retry: Value) -> Result<Value> {
    let mut spec = json!({"kind":"command","command":["echo","controlled"],"resources":{"cpus":1,"memoryMb":256}});
    if !retry.is_null() {
        spec["retry"] = retry;
    }
    c.post("/v1/jobs", json!({"spec":spec})).await
}
async fn phase(socket: &mut Socket, assignment: &Value, value: &str) -> Result<()> {
    send(socket,json!({"type":"job.state","jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"phase":value,"ts":state::now()})).await
}
fn result(assignment: &Value, outcome: &str) -> Value {
    json!({"jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"outcome":outcome,"exitCode":if outcome=="succeeded"{0}else{1},"error":if outcome=="succeeded"{Value::Null}else{json!("controlled failure")},"finishedAt":state::now(),"logSeq":0})
}
async fn wait_state(c: &Client, id: &str, want: &str) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = c.get(&format!("/v1/jobs/{id}")).await?;
            if job["state"] == want {
                return Ok::<_, anyhow::Error>(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for job {id} state {want}"))?
}
async fn closed(socket: &mut Socket) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                _ => {}
            }
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn reconnect_lost_and_failed_results_share_retry_budget() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "retry-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, json!({"max":1,"on":["lost","failed"]})).await?;
    let assignment = receive(&mut socket, "job.assign").await?;
    phase(&mut socket, &assignment, "running").await?;
    wait_state(c, job["id"].as_str().unwrap(), "running").await?;
    socket.close(None).await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let retry = receive(&mut socket, "job.assign").await?;
    let lost = wait_state(c, job["id"].as_str().unwrap(), "lost").await?;
    assert_eq!(lost["stateReason"], Value::Null);
    let retried = c
        .get(&format!("/v1/jobs/{}", retry["jobId"].as_str().unwrap()))
        .await?;
    assert_eq!(retried["retryOf"], job["id"]);
    assert_eq!(retried["attempt"], 2);
    assert_eq!(retried["owner"], "local-admin");
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&retry,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let jobs = c.get("/v1/jobs").await?;
    assert_eq!(
        jobs["jobs"].as_array().unwrap().len(),
        2,
        "lost retry must consume the same budget as results"
    );
    let second = submit(c, json!({"max":1,"on":["failed"]})).await?;
    let assigned = receive(&mut socket, "job.assign").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&assigned,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let retry = receive(&mut socket, "job.assign").await?;
    let retry_view = c
        .get(&format!("/v1/jobs/{}", retry["jobId"].as_str().unwrap()))
        .await?;
    assert_eq!(retry_view["retryOf"], second["id"]);
    assert_eq!(retry_view["attempt"], 2);
    Ok(())
}

#[tokio::test]
async fn cancellation_intent_is_idempotent_success_wins_and_timeout_clears_error() -> Result<()> {
    let oracle: Value = serde_json::from_str(include_str!("../contracts/cancel-race.json"))?;
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "cancel-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, Value::Null).await?;
    let assignment = receive(&mut socket, "job.assign").await?;
    phase(&mut socket, &assignment, "running").await?;
    let path = format!("/v1/jobs/{}/cancel", job["id"].as_str().unwrap());
    let first = c.post(&path, json!({})).await?;
    let again = c.post(&path, json!({})).await?;
    assert_eq!(first["cancelRequestedAt"], again["cancelRequestedAt"]);
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&assignment,"succeeded")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let completed = wait_state(c, job["id"].as_str().unwrap(), "succeeded").await?;
    for key in ["state", "exitCode", "error"] {
        assert_eq!(completed[key], oracle["expected"][key], "frozen race {key}");
    }
    assert_eq!(
        json!(!completed["cancelRequestedAt"].is_null()),
        oracle["expected"]["cancelRequested"]
    );
    assert_eq!(
        json!(first["cancelRequestedAt"] == again["cancelRequestedAt"]),
        oracle["expected"]["repeatedCancelPreservesTimestamp"]
    );
    assert!(
        c.post(&path, json!({}))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("409")
    );
    let events = c
        .get(&format!("/v1/jobs/{}/events", job["id"].as_str().unwrap()))
        .await?;
    assert_eq!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == "cancel-requested")
            .count(),
        oracle["expected"]["cancelEventCount"].as_u64().unwrap() as usize
    );
    let job = submit(c, Value::Null).await?;
    let assignment = receive(&mut socket, "job.assign").await?;
    phase(&mut socket, &assignment, "running").await?;
    let path = format!("/v1/jobs/{}/cancel", job["id"].as_str().unwrap());
    c.post(&path, json!({})).await?;
    let finished = wait_state(c, job["id"].as_str().unwrap(), "cancelled").await?;
    assert!(finished["error"].is_null());
    assert!(finished["stateReason"].is_null());
    assert_eq!(c.post(&path, json!({})).await?["state"], "cancelled");
    Ok(())
}

#[tokio::test]
async fn both_worker_and_assignment_identity_fence_every_report_and_release_once() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let owner = register(c, "fenced-owner").await?;
    let foreign = register(c, "fenced-foreign").await?;
    let mut socket = connect(c, &owner, json!([]), json!([])).await?;
    let mut attacker = connect(c, &foreign, json!([]), json!([])).await?;
    let job = c
        .post(
            "/v1/jobs",
            json!({"spec":{"kind":"command","command":["true"],
        "target":{"node":owner["nodeId"]},"resources":{"cpus":1,"memoryMb":256}}}),
        )
        .await?;
    let first = receive(&mut socket, "job.assign").await?;
    let view = format!("/v1/jobs/{}", job["id"].as_str().unwrap());
    let node = format!("/v1/nodes/{}", owner["nodeId"].as_str().unwrap());
    // The foreign worker knows the current assignment ID. Node ownership must
    // independently reject its state, logs, outputs and result in wire order.
    phase(&mut attacker, &first, "running").await?;
    send(
        &mut attacker,
        json!({"type":"job.log","jobId":first["jobId"],"assignmentId":first["assignmentId"],
        "entries":[{"s":1,"t":state::now(),"o":"stdout","d":"foreign"}]}),
    )
    .await?;
    send(
        &mut attacker,
        json!({"type":"job.outputs","jobId":first["jobId"],"assignmentId":first["assignmentId"],
        "outputs":{"steps":[]}}),
    )
    .await?;
    send(
        &mut attacker,
        json!({"type":"job.result","result":result(&first,"failed")}),
    )
    .await?;
    receive(&mut attacker, "job.ack").await?;
    let unchanged = c.get(&view).await?;
    assert_eq!(unchanged["state"], "assigned");
    assert_eq!(unchanged["logSeq"], 0);
    assert!(unchanged["outputs"].is_null());
    assert_eq!(c.get(&node).await?["allocated"]["cpus"], 1.0);

    phase(&mut socket, &first, "running").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&first,"succeeded")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    wait_state(c, job["id"].as_str().unwrap(), "succeeded").await?;
    assert_eq!(c.get(&node).await?["allocated"]["cpus"], 0.0);
    let retry = c.post(&format!("{view}/retry"), json!({})).await?;
    let second = receive(&mut socket, "job.assign").await?;
    assert_ne!(second["assignmentId"], first["assignmentId"]);
    // The owner now sends the previous attempt's ID, and a correct job ID with
    // a wrong assignment ID. Neither may change the new attempt or reservations.
    phase(&mut socket, &first, "running").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&first,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let mut wrong = second.clone();
    wrong["assignmentId"] = first["assignmentId"].clone();
    phase(&mut socket, &wrong, "running").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&wrong,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let retried = c
        .get(&format!("/v1/jobs/{}", retry["id"].as_str().unwrap()))
        .await?;
    assert_eq!(retried["state"], "assigned");
    assert_eq!(retried["assignmentId"], second["assignmentId"]);
    assert_eq!(c.get(&view).await?["state"], "succeeded");
    assert_eq!(c.get(&node).await?["allocated"]["cpus"], 1.0);
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&second,"succeeded")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&second,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    assert_eq!(
        c.get(&format!("/v1/jobs/{}", retry["id"].as_str().unwrap()))
            .await?["state"],
        "succeeded"
    );
    let released = c.get(&node).await?;
    assert_eq!(released["allocated"]["cpus"], 0.0);
    assert_eq!(released["allocated"]["memoryMb"], 0);
    assert!(released["activeJobs"].as_array().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn large_archives_apply_backpressure_without_starving_heartbeats_or_cancellation()
-> Result<()> {
    use std::io::Read;
    use std::time::Instant;

    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let bytes = 128 * 1024 * 1024;
    let root = cluster.temp.path().to_owned();
    let digest = tokio::task::spawn_blocking(move || -> Result<String> {
        let file = std::fs::File::create(root.join("large-blob"))?;
        file.set_len(bytes)?;
        let (_, hash) = gridstator::release::hash_file(root.join("large-blob"))?;
        let digest = format!("sha256:{hash}");
        let store = gridstator::workspace::BlobStore::new(root.join("controller/blobs"));
        store.put(&digest, std::io::repeat(0).take(bytes), Some(bytes))?;
        Ok(digest)
    })
    .await??;
    let agent = register(c, "backpressure-worker-model").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, Value::Null).await?;
    let assignment = receive(&mut socket, "job.assign").await?;
    phase(&mut socket, &assignment, "running").await?;
    wait_state(c, job["id"].as_str().unwrap(), "running").await?;

    fn resident_kib() -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
    }
    let before = resident_kib();
    let mut stalled = Vec::new();
    // Each response advertises a128MiB tar member. With all four I/O permits held,
    // bounded pipes and network backpressure must prevent four full archives in RAM.
    for _ in 0..4 {
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            c.http
                .post(format!("{}/v1/blobs/download", c.config.url))
                .bearer_auth(&c.config.token)
                .json(&json!({"digests":[digest]}))
                .send(),
        )
        .await??;
        assert!(response.status().is_success());
        stalled.push(response);
    }
    let http = c.http.clone();
    let url = c.config.url.clone();
    let token = c.config.token.clone();
    let requested = digest.clone();
    let mut fifth = tokio::spawn(async move {
        http.post(format!("{url}/v1/blobs/download"))
            .bearer_auth(token)
            .json(&json!({"digests":[requested]}))
            .send()
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut fifth)
            .await
            .is_err(),
        "the fifth archive bypassed the four-transfer admission bound"
    );
    let mut longest = Duration::ZERO;
    for sequence in 0..10 {
        let started = Instant::now();
        let ts = state::now() + sequence;
        send(&mut socket, json!({"type":"heartbeat","activeJobs":[job["id"]],"metrics":{
            "ts":ts,"cpuPercent":1,"memoryAvailableMb":2048,"diskFreeMb":4096,"diskTotalMb":8192,"load1":0
        }})).await?;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                assert_eq!(c.get("/v1/health").await?["ok"], true);
                let node = c
                    .get(&format!("/v1/nodes/{}", agent["nodeId"].as_str().unwrap()))
                    .await?;
                if node["metrics"]["ts"] == ts {
                    assert_eq!(node["status"], "online");
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .context("heartbeat/health probe during stalled archive streams")??;
        longest = longest.max(started.elapsed());
    }
    let started = Instant::now();
    c.post(
        &format!("/v1/jobs/{}/cancel", job["id"].as_str().unwrap()),
        json!({}),
    )
    .await?;
    receive(&mut socket, "job.cancel").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&assignment,"cancelled")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    wait_state(c, job["id"].as_str().unwrap(), "cancelled").await?;
    let cancellation_elapsed = started.elapsed();
    assert!(
        cancellation_elapsed < Duration::from_secs(1),
        "archive backpressure delayed cancellation"
    );
    let growth = before
        .zip(resident_kib())
        .map(|(before, after)| after.saturating_sub(before));
    if let Some(growth) = growth {
        assert!(
            growth < 48 * 1024,
            "four stalled128MiB archives grew RSS by {growth}KiB"
        );
    }
    drop(stalled);
    drop(
        tokio::time::timeout(Duration::from_secs(2), fifth)
            .await
            .context("cancelled streams did not release archive permits")???,
    );
    // Cancellation must release the blocked writers/permits, and the next full
    // streaming download must verify and publish exactly the original content.
    let store = gridstator::workspace::BlobStore::new(cluster.temp.path().join("download"));
    store.init()?;
    let mut full_transfer_probes = 0;
    tokio::time::timeout(Duration::from_secs(20), async {
        let download = gridstator::transfer::download_blobs(c, &store, std::slice::from_ref(&digest));
        tokio::pin!(download);
        let mut interval = tokio::time::interval(Duration::from_millis(25));
        loop {
            tokio::select! {
                result = &mut download => return result,
                _ = interval.tick() => {
                    let started = Instant::now();
                    tokio::time::timeout(Duration::from_secs(1), c.get("/v1/health")).await.context("health probe during full archive transfer")??;
                    send(&mut socket, json!({"type":"heartbeat","activeJobs":[],"metrics":{
                        "ts":state::now(),"cpuPercent":1,"memoryAvailableMb":2048,"diskFreeMb":4096,"diskTotalMb":8192,"load1":0
                    }})).await?;
                    longest = longest.max(started.elapsed());
                    full_transfer_probes += 1;
                }
            }
        }
    }).await.context("full128MiB verified archive download")??;
    assert!(
        full_transfer_probes > 0,
        "no health/heartbeat probe overlapped the full archive transfer"
    );
    assert_eq!(std::fs::metadata(store.path(&digest)?)?.len(), bytes);
    let path = store.path(&digest)?;
    let (_, actual) =
        tokio::task::spawn_blocking(move || gridstator::release::hash_file(path)).await??;
    assert_eq!(format!("sha256:{actual}"), digest);
    println!(
        "128MiB archive; four stalled streams; ten stalled +{full_transfer_probes} full-transfer heartbeat/health probes; max probe {longest:?}; RSS growth {growth:?}KiB; cancellation {cancellation_elapsed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn assigned_rejection_expires_and_stale_results_are_acknowledged() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "reject-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, Value::Null).await?;
    let first = receive(&mut socket, "job.assign").await?;
    send(&mut socket,json!({"type":"job.rejected","jobId":first["jobId"],"assignmentId":first["assignmentId"],"reason":"controlled busy"})).await?;
    let queued = wait_state(c, job["id"].as_str().unwrap(), "queued").await?;
    for key in ["nodeId", "nodeName", "assignmentId", "assignedAt"] {
        assert!(queued[key].is_null(), "{key} retained after rejection");
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(70),
            receive(&mut socket, "job.assign")
        )
        .await
        .is_err(),
        "excluded worker was immediately selected"
    );
    let second = receive(&mut socket, "job.assign").await?;
    assert_ne!(first["assignmentId"], second["assignmentId"]);
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&first,"succeeded")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    assert_eq!(
        c.get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["assignmentId"],
        second["assignmentId"]
    );
    let mut unknown = result(&first, "lost");
    unknown["jobId"] = json!("already-retained-away");
    send(&mut socket, json!({"type":"job.result","result":unknown})).await?;
    assert_eq!(
        receive(&mut socket, "job.ack").await?["jobId"],
        "already-retained-away"
    );
    Ok(())
}

#[tokio::test]
async fn temporary_native_update_admission_does_not_trigger_long_executor_exclusion() -> Result<()>
{
    for reason in [
        "agent is checking an update",
        "agent is draining or checking an update",
    ] {
        let mut configuration = timing();
        configuration.rejection_ttl_ms = 60_000;
        let cluster = boot_with(configuration).await?;
        let client = &cluster.client;
        let agent = register(client, "update-admission-worker").await?;
        let mut socket = connect(client, &agent, json!([]), json!([])).await?;
        let job = submit(client, Value::Null).await?;
        let first = receive(&mut socket, "job.assign").await?;
        send(&mut socket,json!({"type":"job.rejected","jobId":first["jobId"],"assignmentId":first["assignmentId"],"reason":reason})).await?;
        let second =
            tokio::time::timeout(Duration::from_secs(5), receive(&mut socket, "job.assign"))
                .await
                .context("temporary update admission used the long executor exclusion")??;
        assert_eq!(second["jobId"], job["id"]);
        assert_ne!(first["assignmentId"], second["assignmentId"]);
    }
    Ok(())
}

#[tokio::test]
async fn newer_socket_fences_old_generation_and_revocation_closes_both_channels() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "socket-worker").await?;
    let mut first = connect(c, &agent, json!([]), json!([])).await?;
    let mut second = connect(c, &agent, json!([]), json!([])).await?;
    closed(&mut first).await?;
    let nodes = c.get("/v1/nodes").await?;
    let view = &nodes["nodes"][0];
    assert_eq!(view["status"], "online");
    assert_eq!(view["allocatable"]["cpus"], 3.0);
    assert_eq!(view["allocatable"]["memoryMb"], 3840);
    let issued = c
        .post("/v1/tokens", json!({"name":"events-viewer"}))
        .await?;
    let mut events = raw_socket(c, "/v1/events", issued["token"].as_str().unwrap()).await?;
    send(
        &mut events,
        json!({"type":"subscribe","topics":["jobs","groups"]}),
    )
    .await?;
    c.request(
        reqwest::Method::DELETE,
        &format!("/v1/tokens/{}", issued["info"]["id"].as_str().unwrap()),
        None,
    )
    .await?;
    closed(&mut events).await?;
    let tokens = c.get("/v1/tokens").await?;
    let credential = tokens["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|token| token["nodeId"] == agent["nodeId"])
        .unwrap();
    c.request(
        reqwest::Method::DELETE,
        &format!("/v1/tokens/{}", credential["id"].as_str().unwrap()),
        None,
    )
    .await?;
    let fatal = receive(&mut second, "error").await?;
    assert_eq!(fatal["code"], "UNAUTHORIZED");
    closed(&mut second).await?;
    Ok(())
}

#[tokio::test]
async fn protocol_mismatch_flushes_fatal_frame_and_outdated_registration_is_rejected() -> Result<()>
{
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "protocol-worker").await?;
    let mut socket = raw_socket(c, "/v1/agent/ws", agent["credential"].as_str().unwrap()).await?;
    send(
        &mut socket,
        json!({"type":"hello","protocolVersion":999,"info":info(),"activeJobs":[],"results":[]}),
    )
    .await?;
    let fatal = receive(&mut socket, "error").await?;
    assert_eq!(fatal["code"], "PROTOCOL_MISMATCH");
    assert_eq!(fatal["fatal"], true);
    closed(&mut socket).await?;
    let token = c
        .post("/v1/tokens", json!({"kind":"join","name":"outdated"}))
        .await?;
    let mut config = c.config.clone();
    config.token = token["token"].as_str().unwrap().into();
    let outdated = Client::new(config)?;
    let mut old = info();
    old["agentVersion"] = json!("0.0.0");
    assert!(
        outdated
            .post("/v1/agents/register", json!({"name":"outdated","info":old}))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn controller_alias_listener_advertises_public_url_and_preserves_identity() -> Result<()> {
    let mut cluster = boot_with(ControllerTiming::default()).await?;
    cluster.server.abort();
    let _ = (&mut cluster.server).await;
    let dir = cluster.temp.path().join("controller");
    // A cancelled listener can still own a joined blocking confirmation/storage
    // operation. Observe its instance lease rather than racing a fixed sleep.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.join("controller.lock"))?;
            if fs2::FileExt::try_lock_exclusive(&lock).is_ok() {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("old controller instance lease did not release")??;
    let mut saved = state::read_json(&dir.join("controller.json"))?;
    saved["aliases"] = json!([{"host":"127.0.0.1","port":0}]);
    saved["publicUrl"] = json!("https://relocated.example:7300");
    state::write_json(&dir.join("controller.json"), &saved)?;
    std::fs::remove_file(dir.join("runtime.json"))?;
    let server = tokio::spawn(controller::start(dir.clone(), None, None));
    let runtime = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(runtime) = state::read_json(&dir.join("runtime.json")) {
                break runtime;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let mut config = cluster.client.config.clone();
    config.url = format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap());
    let primary = Client::new(config.clone())?;
    let alias_port = runtime["aliases"][0]["port"].as_u64().unwrap();
    config.url = format!("https://127.0.0.1:{alias_port}");
    let alias = Client::new(config)?;
    let result = async {
        let a = primary.get("/v1/cluster").await?;
        let b = alias.get("/v1/cluster").await?;
        assert_eq!(a["clusterId"], b["clusterId"]);
        assert_eq!(a["caFingerprint"], b["caFingerprint"]);
        let agent = register(&primary, "alias-worker").await?;
        let mut socket = raw_socket(
            &alias,
            "/v1/agent/ws",
            agent["credential"].as_str().unwrap(),
        )
        .await?;
        send(
            &mut socket,
            json!({"type":"hello","protocolVersion":1,"info":info(),"activeJobs":[],"results":[]}),
        )
        .await?;
        assert_eq!(
            receive(&mut socket, "welcome").await?["controllerUrl"],
            "https://relocated.example:7300"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    server.abort();
    result
}

#[tokio::test]
async fn timeout_reassigns_unacknowledged_assignment_and_heartbeat_loss_retries_running()
-> Result<()> {
    let mut time = timing();
    time.assign_timeout_ms = 500;
    time.heartbeat_timeout_ms = 5_000;
    let cluster = boot_with(time).await?;
    let c = &cluster.client;
    let agent = register(c, "timeout-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, json!({"max":1,"on":["lost"]})).await?;
    let first = receive(&mut socket, "job.assign").await?;
    let second = receive(&mut socket, "job.assign").await?;
    assert_ne!(first["assignmentId"], second["assignmentId"]);
    phase(&mut socket, &second, "running").await?;
    let lost = wait_state(c, job["id"].as_str().unwrap(), "lost").await?;
    assert!(lost["finishedAt"].as_i64().is_some());
    closed(&mut socket).await?;
    let jobs = c.get("/v1/jobs").await?;
    let retry = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["retryOf"] == job["id"])
        .context("lost retry not created")?;
    assert_eq!(retry["state"], "queued");
    assert_eq!(retry["attempt"], 2);
    assert_eq!(c.get("/v1/nodes").await?["nodes"][0]["status"], "offline");
    Ok(())
}

#[tokio::test]
async fn late_starting_rejection_is_diagnostic_noop_and_force_removal_cancels() -> Result<()> {
    // This case tests rejection and removal, not worker-loss timers. Keep the worker alive while
    // the test observes intermediate states on loaded CI hosts.
    let mut test_timing = timing();
    test_timing.startup_grace_ms = 20_000;
    test_timing.lost_timeout_ms = 20_000;
    let cluster = boot_with(test_timing).await?;
    let c = &cluster.client;
    let agent = register(c, "removed-worker_1").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let job = submit(c, Value::Null).await?;
    let assignment = receive(&mut socket, "job.assign").await?;
    phase(&mut socket, &assignment, "starting").await?;
    wait_state(c, job["id"].as_str().unwrap(), "starting").await?;
    send(&mut socket,json!({"type":"job.rejected","jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"reason":"late rejection"})).await?;
    let event_path = format!("/v1/jobs/{}/events", job["id"].as_str().unwrap());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = c.get(&event_path).await?;
            if events["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "rejection-ignored")
            {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("waiting for late rejection diagnostic event")??;
    assert_eq!(
        c.get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["state"],
        "starting"
    );
    let path = "/v1/nodes/removed%2Dworker_1".to_owned();
    assert_eq!(c.get(&path).await?["id"], agent["nodeId"]);
    assert_eq!(
        c.get("/v1/nodes/removed-worker_1").await?["id"],
        agent["nodeId"]
    );
    assert!(
        c.request(reqwest::Method::DELETE, &path, None)
            .await
            .unwrap_err()
            .to_string()
            .starts_with("409")
    );
    c.request(reqwest::Method::DELETE, &format!("{path}?force=true"), None)
        .await?;
    assert_eq!(receive(&mut socket, "error").await?["code"], "NODE_REMOVED");
    closed(&mut socket).await?;
    wait_state(c, job["id"].as_str().unwrap(), "cancelled").await?;
    assert!(
        c.get("/v1/nodes").await?["nodes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn groups_wait_for_all_shards_retry_only_failed_and_emit_live_updates() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "group-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let mut events = raw_socket(c, "/v1/events", &c.config.token).await?;
    send(&mut events, json!({"type":"subscribe","topics":["jobs"]})).await?;
    let group=c.post("/v1/groups",json!({"name":"shards","shards":2,"spec":{"kind":"command","command":["echo","{shard}/{total}"],"secretEnv":{"TOKEN":"private"},"resources":{"cpus":1,"memoryMb":256}}})).await?;
    let id = group["id"].as_str().unwrap();
    assert_eq!(group["counts"]["queued"], 2);
    assert_eq!(group["jobs"][0]["spec"]["secretEnv"]["TOKEN"], "***");
    let first = receive(&mut socket, "job.assign").await?;
    let failed_index = first["spec"]["env"]["GRIDSTATOR_SHARD_INDEX"]
        .as_str()
        .unwrap()
        .parse::<usize>()?
        - 1;
    assert_eq!(
        first["spec"]["command"][1],
        format!("{}/2", failed_index + 1)
    );
    phase(&mut socket, &first, "running").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&first,"failed")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let second = receive(&mut socket, "job.assign").await?;
    let mixed = c.get(&format!("/v1/groups/{id}")).await?;
    assert_eq!(mixed["state"], "running");
    assert!(mixed["finishedAt"].is_null());
    assert!(
        c.post(&format!("/v1/groups/{id}/retry"), json!({}))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("409")
    );
    phase(&mut socket, &second, "running").await?;
    send(
        &mut socket,
        json!({"type":"job.result","result":result(&second,"succeeded")}),
    )
    .await?;
    receive(&mut socket, "job.ack").await?;
    let done = c.get(&format!("/v1/groups/{id}")).await?;
    assert_eq!(done["state"], "failed");
    assert_eq!(done["counts"]["failed"], 1);
    assert_eq!(done["counts"]["succeeded"], 1);
    assert_eq!(done["nodes"], json!(["group-worker"]));
    assert!(done["startedAt"].as_i64().is_some());
    assert!(done["finishedAt"].as_i64().is_some());
    let terminal_event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = receive(&mut events, "group.updated").await?;
            if update["group"]["state"] == "failed" {
                break Ok::<_, anyhow::Error>(update);
            }
        }
    })
    .await??;
    assert_eq!(terminal_event["group"]["id"], id);
    assert_eq!(terminal_event["group"]["counts"]["failed"], 1);
    // The source schema permits only jobs/nodes; an invalid subscription is ignored in full.
    send(&mut events, json!({"type":"subscribe","topics":["groups"]})).await?;
    let retried = c.post(&format!("/v1/groups/{id}/retry"), json!({})).await?;
    assert_eq!(retried["jobs"][failed_index]["attempt"], 2);
    assert_eq!(retried["jobs"][1 - failed_index]["attempt"], 1);
    assert_eq!(retried["state"], "running");
    let update = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = receive(&mut events, "group.updated").await?;
            if update["group"]["jobs"][failed_index]["attempt"] == 2 {
                break Ok::<_, anyhow::Error>(update);
            }
        }
    })
    .await??;
    assert_eq!(update["group"]["id"], id);
    assert_eq!(c.get("/v1/groups?limit=1").await?["groups"][0]["id"], id);
    Ok(())
}

#[tokio::test]
async fn historical_log_subscription_pages_beyond_ten_thousand_before_end() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let job = submit(c, Value::Null).await?;
    let id = job["id"].as_str().unwrap().to_owned();
    let selected = id.clone();
    let database = gridstator::db::Db::open(&cluster.temp.path().join("controller/controller.db"))?;
    database
        .write(move |connection| {
            let mut job = gridstator::db::get(connection, "jobs", &selected)?.context("job")?;
            job["state"] = json!("succeeded");
            job["finishedAt"] = json!(state::now());
            job["logSeq"] = json!(10_001);
            gridstator::db::put(connection, "jobs", &selected, &job)?;
            let mut statement =
                connection.prepare("INSERT INTO logs(job_id,seq,data)VALUES(?1,?2,?3)")?;
            for seq in 1..=10_001 {
                let entry = json!({"s":seq,"t":state::now(),"o":"stdout","d":"line\n"});
                statement.execute(rusqlite::params![selected, seq, entry.to_string()])?;
            }
            Ok(())
        })
        .await?;
    let mut socket = raw_socket(c, "/v1/events", &c.config.token).await?;
    send(
        &mut socket,
        json!({"type":"logs.subscribe","jobId":id,"afterSeq":0}),
    )
    .await?;
    let count = tokio::time::timeout(Duration::from_secs(10), async {
        let mut last = 0;
        loop {
            let frame = socket.next().await.context("historical logs ended")??;
            let value: Value = serde_json::from_str(frame.to_text()?)?;
            if value["type"] == "logs" {
                for entry in value["entries"].as_array().unwrap() {
                    last += 1;
                    assert_eq!(entry["s"], last);
                }
            } else if value["type"] == "logs.end" {
                assert_eq!(value["state"], "succeeded");
                break Ok::<_, anyhow::Error>(last);
            }
        }
    })
    .await??;
    assert_eq!(count, 10_001);
    Ok(())
}

#[tokio::test]
async fn preferred_nodes_rejection_and_draining_preserve_source_placement() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let a = register(c, "a").await?;
    let b = register(c, "b").await?;
    let mut on_a = connect(c, &a, json!([]), json!([])).await?;
    let mut on_b = connect(c, &b, json!([]), json!([])).await?;
    let preferred = c.post("/v1/jobs", json!({"spec":{"kind":"command","command":["true"],"resources":{"cpus":1,"memoryMb":256},"target":{"prefer":["b"]}}})).await?;
    let assignment = receive(&mut on_b, "job.assign").await?;
    assert_eq!(assignment["jobId"], preferred["id"]);
    send(
        &mut on_b,
        json!({"type":"job.result","result":result(&assignment,"succeeded")}),
    )
    .await?;
    receive(&mut on_b, "job.ack").await?;
    wait_state(c, preferred["id"].as_str().unwrap(), "succeeded").await?;

    let rejected = submit(c, Value::Null).await?;
    let initial = receive(&mut on_a, "job.assign").await?;
    send(&mut on_a, json!({"type":"job.rejected","jobId":initial["jobId"],"assignmentId":initial["assignmentId"],"reason":"source-controlled busy"})).await?;
    let reassigned = receive(&mut on_b, "job.assign").await?;
    assert_eq!(reassigned["jobId"], rejected["id"]);
    assert_ne!(reassigned["assignmentId"], initial["assignmentId"]);
    send(
        &mut on_b,
        json!({"type":"job.result","result":result(&reassigned,"succeeded")}),
    )
    .await?;
    receive(&mut on_b, "job.ack").await?;
    wait_state(c, rejected["id"].as_str().unwrap(), "succeeded").await?;

    for node in [&a, &b] {
        c.request(
            reqwest::Method::PATCH,
            &format!("/v1/nodes/{}", node["nodeId"].as_str().unwrap()),
            Some(json!({"schedulingMode":"draining"})),
        )
        .await?;
    }
    let waiting = submit(c, Value::Null).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        c.get(&format!("/v1/jobs/{}", waiting["id"].as_str().unwrap()))
            .await?["state"],
        "queued"
    );
    c.request(
        reqwest::Method::PATCH,
        &format!("/v1/nodes/{}", a["nodeId"].as_str().unwrap()),
        Some(json!({"schedulingMode":"enabled"})),
    )
    .await?;
    assert_eq!(
        receive(&mut on_a, "job.assign").await?["jobId"],
        waiting["id"]
    );
    Ok(())
}

#[tokio::test]
async fn source_group_spreading_placeholders_and_cancellation_are_observable() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let a = register(c, "a").await?;
    let b = register(c, "b").await?;
    let mut on_a = connect(c, &a, json!([]), json!([])).await?;
    let mut on_b = connect(c, &b, json!([]), json!([])).await?;
    let group = c.post("/v1/groups",json!({"shards":2,"spec":{"kind":"command","command":["sh","-c","run --shard={shard}/{total}"],"resources":{"cpus":1,"memoryMb":256}}})).await?;
    let id = group["id"].as_str().unwrap();
    assert_eq!(group["shardTotal"], 2);
    for (index, job) in group["jobs"].as_array().unwrap().iter().enumerate() {
        assert_eq!(job["shardIndex"], index + 1);
        assert_eq!(job["groupId"], id);
        assert_eq!(job["shardTotal"], 2);
    }
    let first = receive(&mut on_a, "job.assign").await?;
    let second = receive(&mut on_b, "job.assign").await?;
    let mut commands = vec![
        first["spec"]["command"][2].as_str().unwrap(),
        second["spec"]["command"][2].as_str().unwrap(),
    ];
    commands.sort_unstable();
    assert_eq!(commands, ["run --shard=1/2", "run --shard=2/2"]);
    assert_eq!(first["spec"]["env"]["GRIDSTATOR_SHARD_TOTAL"], "2");
    phase(&mut on_a, &first, "running").await?;
    phase(&mut on_b, &second, "running").await?;
    let cancelled = c
        .post(&format!("/v1/groups/{id}/cancel"), json!({}))
        .await?;
    assert_eq!(cancelled["shardTotal"], 2);
    for (socket, assignment) in [(&mut on_a, &first), (&mut on_b, &second)] {
        assert_eq!(
            receive(socket, "job.cancel").await?["jobId"],
            assignment["jobId"]
        );
        send(
            socket,
            json!({"type":"job.result","result":result(assignment,"cancelled")}),
        )
        .await?;
        receive(socket, "job.ack").await?;
        wait_state(c, assignment["jobId"].as_str().unwrap(), "cancelled").await?;
    }
    let done = c.get(&format!("/v1/groups/{id}")).await?;
    assert_eq!(done["state"], "cancelled");
    assert_eq!(done["counts"]["cancelled"], 2);
    assert!(
        c.post(
            "/v1/groups",
            json!({"shards":2,"spec":{"kind":"docker-build","dockerfile":"Dockerfile"}})
        )
        .await
        .unwrap_err()
        .to_string()
        .starts_with("400")
    );
    Ok(())
}

#[tokio::test]
async fn unknown_snapshot_and_target_are_rejected_before_queuing() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    for (fields, status) in [
        (
            json!({"snapshot":format!("sha256:{}","0".repeat(64))}),
            "400",
        ),
        (json!({"target":{"node":"ghost"}}), "404"),
    ] {
        let mut spec = json!({"kind":"command","command":["true"]});
        spec.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(
            c.post("/v1/jobs", json!({"spec":spec}))
                .await
                .unwrap_err()
                .to_string()
                .starts_with(status)
        );
    }
    assert!(
        c.get("/v1/jobs").await?["jobs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn live_steps_agent_artifacts_and_stale_output_match_source_contract() -> Result<()> {
    let cluster = boot_with(timing()).await?;
    let c = &cluster.client;
    let agent = register(c, "step-worker").await?;
    let mut socket = connect(c, &agent, json!([]), json!([])).await?;
    let mut config = c.config.clone();
    config.token = agent["credential"].as_str().unwrap().to_owned();
    let worker = Client::new(config)?;
    for duration in [1200, -1] {
        let job = submit(c, Value::Null).await?;
        let assignment = receive(&mut socket, "job.assign").await?;
        phase(&mut socket, &assignment, "running").await?;
        let mut step = json!({"id":"s1","group":"CI/test","name":"Run tests","status":"running","startedAt":state::now(),"durationMs":null});
        send(&mut socket,json!({"type":"job.outputs","jobId":job["id"],"assignmentId":assignment["assignmentId"],"outputs":{"steps":[step]}})).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if c.get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
                    .await?["outputs"]["steps"][0]["name"]
                    == "Run tests"
                {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        step["status"] = json!("success");
        step["durationMs"] = json!(duration);
        send(&mut socket,json!({"type":"job.outputs","jobId":job["id"],"assignmentId":assignment["assignmentId"],"outputs":{"steps":[step]}})).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if c.get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
                    .await?["outputs"]["steps"][0]["durationMs"]
                    == duration.max(0)
                {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let content = b"<testsuites/>";
        let digest = gridstator::workspace::digest_of(content);
        let missing = worker
            .post("/v1/blobs/missing", json!({"digests":[digest]}))
            .await?;
        if duration == 1200 {
            assert_eq!(missing["missing"], json!([digest]));
        }
        let mut archive = tar::Builder::new(vec![]);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        archive.append_data(
            &mut header,
            format!("sha256/{}", digest.trim_start_matches("sha256:")),
            content.as_slice(),
        )?;
        worker
            .http
            .post(format!("{}/v1/blobs/upload", c.config.url))
            .bearer_auth(&worker.config.token)
            .header("content-type", "application/x-tar")
            .body(archive.into_inner()?)
            .send()
            .await?
            .error_for_status()?;
        let mut terminal = result(&assignment, "succeeded");
        terminal["outputs"] = json!({"steps":[step]});
        terminal["artifacts"] =
            json!([{"name":"reports/junit.xml","digest":digest,"size":content.len()}]);
        send(&mut socket, json!({"type":"job.result","result":terminal})).await?;
        receive(&mut socket, "job.ack").await?;
        let done = wait_state(c, job["id"].as_str().unwrap(), "succeeded").await?;
        assert_eq!(done["outputs"]["steps"][0]["status"], "success");
        assert_eq!(done["outputs"]["steps"][0]["durationMs"], duration.max(0));
        assert_eq!(done["artifacts"], terminal["artifacts"]);
        let base = format!("/v1/jobs/{}", job["id"].as_str().unwrap());
        assert_eq!(
            c.get(&format!("{base}/artifacts")).await?["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let downloaded = c
            .http
            .get(format!(
                "{}{base}/artifacts/reports/junit.xml",
                c.config.url
            ))
            .bearer_auth(&c.config.token)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        assert_eq!(downloaded.as_ref(), content);
        assert_eq!(
            c.http
                .get(format!("{}{base}/artifacts/nope.txt", c.config.url))
                .bearer_auth(&c.config.token)
                .send()
                .await?
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
        send(&mut socket,json!({"type":"job.outputs","jobId":job["id"],"assignmentId":"stale","outputs":{"steps":[]}})).await?;
        // An acknowledged following request places the stale frame before this observation.
        send(&mut socket, json!({"type":"job.result","result":terminal})).await?;
        receive(&mut socket, "job.ack").await?;
        assert_eq!(
            c.get(&base).await?["outputs"]["steps"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn source_followers_receive_live_logs_then_terminal_end_without_duplicate_entries()
-> Result<()> {
    for outcome in ["succeeded", "failed"] {
        let cluster = boot_with(timing()).await?;
        let c = &cluster.client;
        let agent = register(c, "live-log-worker").await?;
        let mut worker = connect(c, &agent, json!([]), json!([])).await?;
        let job = submit(c, Value::Null).await?;
        let assignment = receive(&mut worker, "job.assign").await?;
        phase(&mut worker, &assignment, "starting").await?;
        phase(&mut worker, &assignment, "running").await?;
        let mut follower = raw_socket(c, "/v1/events", &c.config.token).await?;
        send(
            &mut follower,
            json!({"type":"logs.subscribe","jobId":job["id"],"afterSeq":0}),
        )
        .await?;
        let entries = json!([{"s":1,"t":state::now(),"o":"stdout","d":"first 🦀\n"},{"s":2,"t":state::now(),"o":"stderr","d":"last partial line"}]);
        let frame = json!({"type":"job.log","jobId":job["id"],"assignmentId":assignment["assignmentId"],"entries":entries});
        send(&mut worker,json!({"type":"job.log","jobId":job["id"],"assignmentId":"stale","entries":[{"s":99,"t":state::now(),"o":"stdout","d":"stale must be ignored"}]})).await?;
        send(&mut worker, frame.clone()).await?;
        let mut received = Vec::new();
        while received.len() < 2 {
            let logs = receive(&mut follower, "logs").await?;
            received.extend(logs["entries"].as_array().unwrap().iter().cloned());
        }
        assert_eq!(Value::Array(received), entries);
        send(&mut worker, frame).await?;
        let mut terminal = result(&assignment, outcome);
        terminal["logSeq"] = json!(2);
        send(&mut worker, json!({"type":"job.result","result":terminal})).await?;
        receive(&mut worker, "job.ack").await?;
        let end = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = follower
                    .next()
                    .await
                    .context("follower closed before logs.end")??;
                let Message::Text(raw) = frame else {
                    continue;
                };
                let message: Value = serde_json::from_str(&raw)?;
                if message["type"] == "logs" {
                    assert!(
                        message["entries"].as_array().unwrap().is_empty(),
                        "duplicate live logs: {message}"
                    );
                } else if message["type"] == "logs.end" {
                    break Ok::<_, anyhow::Error>(message);
                }
            }
        })
        .await??;
        assert_eq!(end["state"], outcome);
        let persisted = c
            .get(&format!("/v1/jobs/{}/logs", job["id"].as_str().unwrap()))
            .await?;
        assert_eq!(persisted["entries"], entries);
        assert_eq!(persisted["complete"], true);
        let contradictory = if outcome == "succeeded" {
            "failed"
        } else {
            "succeeded"
        };
        let mut duplicate = result(&assignment, contradictory);
        duplicate["logSeq"] = json!(2);
        send(&mut worker, json!({"type":"job.result","result":duplicate})).await?;
        receive(&mut worker, "job.ack").await?;
        assert_eq!(
            c.get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
                .await?["state"],
            outcome
        );
        let audit = c
            .get(&format!("/v1/jobs/{}/events", job["id"].as_str().unwrap()))
            .await?;
        let states: Vec<_> = audit["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["toState"].as_str())
            .collect();
        assert_eq!(
            states,
            [
                "created", "queued", "assigned", "starting", "running", outcome
            ]
        );
    }
    Ok(())
}
