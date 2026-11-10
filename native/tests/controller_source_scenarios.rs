//! Remaining frozen controller scenarios, against real loopback TLS/WebSocket listeners.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use gridstator::{
    client::{Client, Config},
    controller::{self, ControllerTiming},
    db, pki, state,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, path::PathBuf, time::Duration};
use tempfile::TempDir;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const WAIT: Duration = Duration::from_secs(10);

fn timing() -> ControllerTiming {
    ControllerTiming {
        heartbeat_timeout_ms: 30_000,
        lost_timeout_ms: 1_200,
        cancel_timeout_ms: 1_000,
        assign_timeout_ms: 10_000,
        startup_grace_ms: 1_200,
        rejection_ttl_ms: 1_000,
        sweep_interval: Duration::from_millis(25),
        schedule_interval: Duration::from_millis(50),
        retention_interval: Duration::from_secs(3600),
    }
}

struct Cluster {
    directory: TempDir,
    server: Option<tokio::task::JoinHandle<Result<()>>>,
    client: Client,
}
impl Drop for Cluster {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}
impl Cluster {
    fn data(&self) -> PathBuf {
        self.directory.path().join("controller")
    }
    async fn boot() -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = TempDir::new()?;
        let data = directory.path().join("controller");
        controller::initialize(
            &data,
            "127.0.0.1",
            0,
            &[],
            false,
            &directory.path().join("client.json"),
        )
        .await?;
        {
            let database = db::Db::open(&data.join("controller.db"))?;
            database
                .write(|connection| {
                    db::put(
                        connection,
                        "settings",
                        "cluster",
                        &json!({"jobRetentionDays":90,"logRetentionDays":30,"snapshotRetentionDays":7,"autoUpdate":"off","updateWindow":null,"toolchain":{"act":"off"}}),
                    )
                })
                .await?;
        }
        let config: Config =
            serde_json::from_slice(&std::fs::read(directory.path().join("client.json"))?)?;
        let mut cluster = Self {
            directory,
            server: None,
            client: Client::new(config)?,
        };
        cluster.start().await?;
        Ok(cluster)
    }
    async fn start(&mut self) -> Result<()> {
        self.server = Some(tokio::spawn(controller::start_with_timing(
            self.data(),
            None,
            Some(0),
            timing(),
        )));
        let runtime = self.data().join("runtime.json");
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(server) = &self.server
                    && server.is_finished()
                {
                    bail!("controller exited before readiness");
                }
                if let Ok(value) = state::read_json(&runtime)
                    && let Some(port) = value["port"].as_u64()
                {
                    let mut config = self.client.config.clone();
                    config.url = format!("https://127.0.0.1:{port}");
                    let client = Client::new(config)?;
                    if client.get("/v1/health").await.is_ok() {
                        self.client = client;
                        return Ok::<_, anyhow::Error>(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn restart(&mut self) -> Result<()> {
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
        // Aborting the listener still owns queued DB operations until their owner exits.
        let lock_path = self.data().join("controller.lock");
        tokio::time::timeout(WAIT, async {
            loop {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)?;
                if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        match std::fs::remove_file(self.data().join("runtime.json")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.start().await
    }
}

fn info() -> Value {
    json!({"hostname":"source-worker","platform":"linux","arch":"x64","osRelease":"test","cpus":8,"memoryTotalMb":16384,"agentVersion":gridstator::VERSION,"docker":{"available":true},"buildx":{"available":true},"act":{"available":false},"executors":["command"]})
}
async fn register(client: &Client, name: &str) -> Result<Value> {
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"source-pairing","maxUses":1}),
        )
        .await?;
    let mut config = client.config.clone();
    config.token = token["token"].as_str().context("join token")?.into();
    Client::new(config)?
        .post(
            "/v1/agents/register",
            json!({"name":name,"info":info(),"settings":{"maxConcurrency":4,"reserved":{"cpus":0,"memoryMb":0}}}),
        )
        .await
}
struct Agent {
    socket: Socket,
    pending: VecDeque<Value>,
}
impl Agent {
    async fn connect(
        client: &Client,
        registration: &Value,
        active: Value,
        results: Value,
    ) -> Result<(Self, Value)> {
        let mut request = format!(
            "{}/v1/agent/ws",
            client.config.url.replacen("https:", "wss:", 1)
        )
        .into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!(
                "Bearer {}",
                registration["credential"].as_str().context("credential")?
            )
            .parse()?,
        );
        let (socket, _) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(pki::websocket_tls(
                &client.config.ca_pem,
            )?)),
        )
        .await?;
        let mut agent = Self {
            socket,
            pending: VecDeque::new(),
        };
        agent.send(json!({"type":"hello","protocolVersion":1,"info":info(),"activeJobs":active,"results":results})).await?;
        let welcome = agent.next("welcome").await?;
        Ok((agent, welcome))
    }
    async fn send(&mut self, message: Value) -> Result<()> {
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await?;
        Ok(())
    }
    async fn next(&mut self, kind: &str) -> Result<Value> {
        tokio::time::timeout(WAIT, async {
            if let Some(index) = self
                .pending
                .iter()
                .position(|message| message["type"] == kind)
            {
                return Ok(self.pending.remove(index).unwrap());
            }
            loop {
                let message = self.socket.next().await.context("agent socket closed")??;
                if message.is_close() {
                    bail!("agent closed while waiting for {kind}");
                }
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text)?;
                    if value["type"] == kind {
                        return Ok(value);
                    }
                    self.pending.push_back(value);
                }
            }
        })
        .await
        .with_context(|| format!("waiting for {kind}"))?
    }
    async fn phase(&mut self, assignment: &Value, phase: &str) -> Result<()> {
        self.send(json!({"type":"job.state","jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"phase":phase,"ts":state::now()})).await
    }
    async fn result(&mut self, result: Value) -> Result<()> {
        let job = result["jobId"].clone();
        self.send(json!({"type":"job.result","result":result}))
            .await?;
        assert_eq!(self.next("job.ack").await?["jobId"], job);
        Ok(())
    }
    async fn close(&mut self) -> Result<()> {
        self.socket.close(None).await?;
        Ok(())
    }
}
fn spec() -> Value {
    json!({"kind":"command","image":"alpine","command":["true"],"resources":{"cpus":1,"memoryMb":512}})
}
fn result(assignment: &Value, outcome: &str) -> Value {
    json!({"jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"outcome":outcome,"exitCode":if outcome=="succeeded"{0}else{1},"error":if outcome=="succeeded"{Value::Null}else{json!("boom")},"oomKilled":false,"finishedAt":state::now(),"logSeq":0})
}
async fn submit(client: &Client, spec: Value) -> Result<Value> {
    client.post("/v1/jobs", json!({"spec":spec})).await
}
async fn wait_job(client: &Client, id: &Value, wanted: &str) -> Result<Value> {
    wait_view(
        client,
        &format!("/v1/jobs/{}", id.as_str().unwrap()),
        wanted,
    )
    .await
}
async fn wait_view(client: &Client, path: &str, wanted: &str) -> Result<Value> {
    tokio::time::timeout(WAIT, async {
        loop {
            let value = client.get(path).await?;
            if value["state"] == wanted {
                return Ok::<_, anyhow::Error>(value);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for {path} state {wanted}"))?
}

#[tokio::test]
async fn rename_preserves_worker_identity_jobs_and_controller_name_on_reconnect() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "DESKTOP-JEL").await?;
    let id = registration["nodeId"].as_str().context("node id")?;
    let path = format!("/v1/nodes/{id}");
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let original = client.get(&path).await?;
    let job = submit(client, spec()).await?;
    let assignment = agent.next("job.assign").await?;
    agent.phase(&assignment, "running").await?;
    wait_job(client, &job["id"], "running").await?;
    let renamed = client
        .request(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"name":"big-box"})),
        )
        .await?;
    assert_eq!(renamed["name"], "big-box");
    assert_eq!(renamed["id"], id);
    assert_eq!(renamed["settings"], original["settings"]);
    assert_eq!(renamed["createdAt"], original["createdAt"]);
    assert_eq!(renamed["connectedAt"], original["connectedAt"]);
    assert_eq!(renamed["info"]["hostname"], "source-worker");
    assert_eq!(client.get("/v1/nodes/big-box").await?["id"], id);
    assert!(
        client
            .get("/v1/nodes/DESKTOP-JEL")
            .await
            .unwrap_err()
            .to_string()
            .starts_with("404")
    );
    let running = wait_job(client, &job["id"], "running").await?;
    assert_eq!(running["nodeId"], id);
    assert_eq!(
        running["nodeName"], "DESKTOP-JEL",
        "Recorded job history keeps the name used for dispatch"
    );
    agent.close().await?;
    let active = json!([{"jobId":job["id"],"assignmentId":assignment["assignmentId"],"phase":"running","logSeq":0}]);
    let (mut agent, welcome) =
        Agent::connect(client, &registration, active.clone(), json!([])).await?;
    assert_eq!(welcome["name"], "big-box");
    assert_eq!(welcome["nodeId"], id);
    agent.send(json!({"type":"hello","protocolVersion":1,"name":"DESKTOP-JEL","info":info(),"activeJobs":active,"results":[]})).await?;
    assert_eq!(agent.next("welcome").await?["name"], "big-box");
    assert_eq!(client.get(&path).await?["name"], "big-box");
    agent.result(result(&assignment, "succeeded")).await?;
    let finished = wait_job(client, &job["id"], "succeeded").await?;
    assert_eq!(finished["nodeId"], id);
    assert_eq!(finished["nodeName"], "DESKTOP-JEL");
    Ok(())
}

#[tokio::test]
async fn rename_validates_unique_names_and_commits_atomically_with_settings() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "old-name").await?;
    let other = register(client, "another-worker").await?;
    let id = registration["nodeId"].as_str().context("node id")?;
    let path = format!("/v1/nodes/{id}");
    let original = client.get(&path).await?;
    for name in [
        json!(""),
        json!("with spaces"),
        json!(".hidden"),
        json!("_hidden"),
        json!("bad/name"),
        json!("é"),
        json!("bad\nname"),
        json!("a".repeat(65)),
        json!(null),
        json!(1),
    ] {
        let response = client
            .http
            .patch(format!("{}{path}", client.config.url))
            .bearer_auth(&client.config.token)
            .json(&json!({"name":name,"maxConcurrency":8}))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "invalid name {name}"
        );
        let unchanged = client.get(&path).await?;
        assert_eq!(unchanged["name"], "old-name");
        assert_eq!(unchanged["settings"], original["settings"]);
    }
    for name in [json!("another-worker"), other["nodeId"].clone()] {
        let response = client
            .http
            .patch(format!("{}{path}", client.config.url))
            .bearer_auth(&client.config.token)
            .json(&json!({"name":name,"maxConcurrency":8}))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::CONFLICT,
            "ambiguous name {name}"
        );
        assert_eq!(client.get(&path).await?["settings"], original["settings"]);
    }
    assert!(
        client
            .request(
                reqwest::Method::PATCH,
                &path,
                Some(json!({"name":"new-name","maxConcurrency":0}))
            )
            .await
            .is_err()
    );
    assert_eq!(client.get(&path).await?["name"], "old-name");
    let renamed = client
        .request(
            reqwest::Method::PATCH,
            "/v1/nodes/old-name",
            Some(json!({"name":"big-box","maxConcurrency":8})),
        )
        .await?;
    assert_eq!(renamed["name"], "big-box");
    assert_eq!(renamed["id"], id);
    assert_eq!(renamed["settings"]["maxConcurrency"], 8);
    assert!(renamed["settings"].get("name").is_none());
    assert_eq!(
        client
            .request(
                reqwest::Method::PATCH,
                &path,
                Some(json!({"name":"big-box"}))
            )
            .await?["name"],
        "big-box"
    );
    Ok(())
}

#[tokio::test]
async fn native_nodes_set_name_renames_a_registered_worker() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let registration = register(&cluster.client, "old-worker").await?;
    let config = cluster.directory.path().join("rename-client.json");
    state::write_json(&config, &serde_json::to_value(&cluster.client.config)?)?;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(["nodes", "set", "old-worker", "--name", "big-box"])
        .env("GRIDSTATOR_CONFIG", &config)
        .env_remove("GRIDSTATOR_URL")
        .env_remove("GRIDSTATOR_TOKEN")
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let renamed = cluster.client.get("/v1/nodes/big-box").await?;
    assert_eq!(renamed["id"], registration["nodeId"]);
    Ok(())
}

#[tokio::test]
async fn rename_preserves_queued_targets_preferences_and_completed_job_retries() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "old-worker").await?;
    register(client, "other-worker").await?;
    let id = registration["nodeId"].as_str().context("node id")?;
    let path = format!("/v1/nodes/{id}");
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let mut hard_spec = spec();
    hard_spec["target"] = json!({"node":"old-worker","prefer":["old-worker","other-worker"]});
    let completed = submit(client, hard_spec.clone()).await?;
    let assignment = agent.next("job.assign").await?;
    agent.result(result(&assignment, "succeeded")).await?;
    wait_job(client, &completed["id"], "succeeded").await?;
    let events_path = format!("/v1/jobs/{}/events", completed["id"].as_str().unwrap());
    let events = client.get(&events_path).await?;
    client
        .request(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"schedulingMode":"disabled"})),
        )
        .await?;
    let queued = submit(client, hard_spec).await?;
    let mut soft_spec = spec();
    soft_spec["target"] = json!({"prefer":["old-worker","other-worker"]});
    let preferred = submit(client, soft_spec).await?;
    let renamed = client
        .request(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"name":"new-worker"})),
        )
        .await?;
    assert_eq!(renamed["settings"]["schedulingMode"], "disabled");
    let notification = loop {
        let message = agent.next("node.settings").await?;
        if message.get("name").is_some() {
            break message;
        }
    };
    assert_eq!(notification["name"], "new-worker");
    assert!(notification["settings"].get("name").is_none());
    for original in [&completed, &queued, &preferred] {
        let updated = client
            .get(&format!("/v1/jobs/{}", original["id"].as_str().unwrap()))
            .await?;
        let mut expected = original["spec"].clone();
        if expected["target"]["node"] == "old-worker" {
            expected["target"]["node"] = json!(id);
        }
        expected["target"]["prefer"] = json!([id, "other-worker"]);
        assert_eq!(updated["spec"], expected);
    }
    assert_eq!(client.get(&events_path).await?, events);
    let done = client
        .get(&format!("/v1/jobs/{}", completed["id"].as_str().unwrap()))
        .await?;
    assert_eq!(done["nodeName"], "old-worker");
    let retry = client
        .post(
            &format!("/v1/jobs/{}/retry", completed["id"].as_str().unwrap()),
            json!({}),
        )
        .await?;
    assert_eq!(retry["spec"]["target"]["node"], id);
    assert_eq!(
        retry["spec"]["target"]["prefer"],
        json!([id, "other-worker"])
    );
    client
        .request(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"schedulingMode":"enabled"})),
        )
        .await?;
    for _ in 0..3 {
        let assignment = agent.next("job.assign").await?;
        agent.result(result(&assignment, "succeeded")).await?;
        let finished = wait_job(client, &assignment["jobId"], "succeeded").await?;
        assert_eq!(finished["nodeId"], id);
        assert_eq!(finished["nodeName"], "new-worker");
    }
    Ok(())
}

#[tokio::test]
async fn source_006_queued_cancel_and_failed_143_result_become_cancelled() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let mut queued = spec();
    queued["target"] = json!({"labels":["nowhere"]});
    let queued = submit(client, queued).await?;
    assert_eq!(
        client
            .post(
                &format!("/v1/jobs/{}/cancel", queued["id"].as_str().unwrap()),
                json!({})
            )
            .await?["state"],
        "cancelled"
    );
    let registration = register(client, "n1").await?;
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let job = submit(client, spec()).await?;
    let assignment = agent.next("job.assign").await?;
    assert_eq!(assignment["jobId"], job["id"]);
    agent.phase(&assignment, "running").await?;
    wait_job(client, &job["id"], "running").await?;
    let pending = client
        .post(
            &format!("/v1/jobs/{}/cancel", job["id"].as_str().unwrap()),
            json!({}),
        )
        .await?;
    assert!(!pending["cancelRequestedAt"].is_null());
    assert_eq!(agent.next("job.cancel").await?["jobId"], job["id"]);
    let mut failed = result(&assignment, "failed");
    failed["exitCode"] = json!(143);
    failed["error"] = json!("killed");
    agent.result(failed).await?;
    let done = wait_job(client, &job["id"], "cancelled").await?;
    assert_eq!(done["exitCode"], 143);
    assert!(done["error"].is_null());
    Ok(())
}

#[tokio::test]
async fn source_008_retry_budget_manual_attempt_three_and_active_conflict() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "n1").await?;
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let mut input = spec();
    input["retry"] = json!({"max":1,"on":["failed"]});
    let first = submit(client, input).await?;
    let assignment = agent.next("job.assign").await?;
    agent.result(result(&assignment, "failed")).await?;
    wait_job(client, &first["id"], "failed").await?;
    let assignment = agent.next("job.assign").await?;
    assert_ne!(assignment["jobId"], first["id"]);
    assert_eq!(assignment["attempt"], 2);
    let automatic = client
        .get(&format!(
            "/v1/jobs/{}",
            assignment["jobId"].as_str().unwrap()
        ))
        .await?;
    assert_eq!(automatic["retryOf"], first["id"]);
    agent.result(result(&assignment, "failed")).await?;
    wait_job(client, &automatic["id"], "failed").await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), agent.next("job.assign"))
            .await
            .is_err(),
        "automatic retry budget was not exhausted"
    );
    let manual = client
        .post(
            &format!("/v1/jobs/{}/retry", automatic["id"].as_str().unwrap()),
            json!({}),
        )
        .await?;
    assert_eq!(manual["attempt"], 3);
    assert_eq!(manual["retryOf"], automatic["id"]);
    assert_eq!(manual["state"], "queued");
    let error = client
        .post(
            &format!("/v1/jobs/{}/retry", manual["id"].as_str().unwrap()),
            json!({}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().starts_with("409"), "{error}");
    Ok(())
}

#[tokio::test]
async fn source_012_disappearance_loses_running_and_requeues_assigned() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "n1").await?;
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let running = submit(client, spec()).await?;
    let assignment = agent.next("job.assign").await?;
    agent.phase(&assignment, "running").await?;
    wait_job(client, &running["id"], "running").await?;
    let assigned = submit(client, spec()).await?;
    assert_eq!(agent.next("job.assign").await?["jobId"], assigned["id"]);
    agent.close().await?;
    let lost = wait_job(client, &running["id"], "lost").await?;
    assert!(lost["error"].as_str().unwrap().contains("Lost contact"));
    let queued = wait_job(client, &assigned["id"], "queued").await?;
    assert!(queued["nodeId"].is_null());
    assert!(queued["assignmentId"].is_null());
    Ok(())
}

#[tokio::test]
async fn source_013_restart_resumes_known_job_cancels_unknown_and_replays_logs() -> Result<()> {
    let mut cluster = Cluster::boot().await?;
    let registration = register(&cluster.client, "n1").await?;
    let (mut agent, _) =
        Agent::connect(&cluster.client, &registration, json!([]), json!([])).await?;
    let keep = submit(&cluster.client, spec()).await?;
    let keeping = agent.next("job.assign").await?;
    let forget = submit(&cluster.client, spec()).await?;
    let forgetting = agent.next("job.assign").await?;
    agent.phase(&keeping, "running").await?;
    agent.phase(&forgetting, "running").await?;
    wait_job(&cluster.client, &keep["id"], "running").await?;
    wait_job(&cluster.client, &forget["id"], "running").await?;
    agent.send(json!({"type":"job.log","jobId":keep["id"],"assignmentId":keeping["assignmentId"],"entries":[{"s":1,"t":1,"o":"stdout","d":"before\n"}]})).await?;
    tokio::time::timeout(WAIT, async {
        loop {
            if cluster
                .client
                .get(&format!("/v1/jobs/{}", keep["id"].as_str().unwrap()))
                .await?["logSeq"]
                == 1
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    agent.close().await?;
    cluster.restart().await?;
    let unknown = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    let (mut agent, welcome) = Agent::connect(
        &cluster.client,
        &registration,
        json!([
            {"jobId":keep["id"],"assignmentId":keeping["assignmentId"],"phase":"running","logSeq":2},
            {"jobId":unknown,"assignmentId":"x","phase":"running","logSeq":0}
        ]),
        json!([]),
    )
    .await?;
    assert_eq!(welcome["resume"], json!([{"jobId":keep["id"],"logSeq":1}]));
    assert_eq!(agent.next("job.cancel").await?["jobId"], unknown);
    let lost = wait_job(&cluster.client, &forget["id"], "lost").await?;
    assert_eq!(
        lost["error"],
        "The agent on n1 restarted or lost track of the job"
    );
    agent.send(json!({"type":"job.log","jobId":keep["id"],"assignmentId":keeping["assignmentId"],"entries":[{"s":1,"t":1,"o":"stdout","d":"before\n"},{"s":2,"t":2,"o":"stdout","d":"during-outage\n"}]})).await?;
    let mut done = result(&keeping, "succeeded");
    done["logSeq"] = json!(2);
    agent.result(done).await?;
    wait_job(&cluster.client, &keep["id"], "succeeded").await?;
    let logs = cluster
        .client
        .get(&format!("/v1/jobs/{}/logs", keep["id"].as_str().unwrap()))
        .await?;
    assert_eq!(
        logs["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["d"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["before\n", "during-outage\n"]
    );
    Ok(())
}

#[tokio::test]
async fn source_014_disconnected_pending_result_resumes_and_is_acknowledged() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let registration = register(client, "n1").await?;
    let (mut agent, _) = Agent::connect(client, &registration, json!([]), json!([])).await?;
    let job = submit(client, spec()).await?;
    let assignment = agent.next("job.assign").await?;
    agent.phase(&assignment, "running").await?;
    wait_job(client, &job["id"], "running").await?;
    agent.close().await?;
    let terminal = result(&assignment, "succeeded");
    let (mut agent, welcome) =
        Agent::connect(client, &registration, json!([]), json!([terminal])).await?;
    assert_eq!(welcome["resume"], json!([{"jobId":job["id"],"logSeq":0}]));
    agent.result(terminal).await?;
    wait_job(client, &job["id"], "succeeded").await?;
    Ok(())
}

#[tokio::test]
async fn source_018_two_node_shards_aggregate_and_retry_only_failed_lineage() -> Result<()> {
    let cluster = Cluster::boot().await?;
    let client = &cluster.client;
    let a = register(client, "a").await?;
    let b = register(client, "b").await?;
    let (mut a, _) = Agent::connect(client, &a, json!([]), json!([])).await?;
    let (mut b, _) = Agent::connect(client, &b, json!([]), json!([])).await?;
    let mut input = spec();
    input["command"] = json!(["sh", "-c", "run --shard={shard}/{total}"]);
    let group = client
        .post("/v1/groups", json!({"spec":input,"shards":2}))
        .await?;
    let id = group["id"].as_str().unwrap();
    assert_eq!(group["shardTotal"], 2);
    assert_eq!(group["state"], "queued");
    assert_eq!(
        group["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| json!([job["shardIndex"], job["shardTotal"], job["groupId"]]))
            .collect::<Vec<_>>(),
        [json!([1, 2, id]), json!([2, 2, id])]
    );
    let on_a = a.next("job.assign").await?;
    let on_b = b.next("job.assign").await?;
    let mut commands = vec![
        on_a["spec"]["command"][2].as_str().unwrap(),
        on_b["spec"]["command"][2].as_str().unwrap(),
    ];
    commands.sort_unstable();
    assert_eq!(commands, ["run --shard=1/2", "run --shard=2/2"]);
    assert_eq!(on_a["spec"]["env"]["GRIDSTATOR_SHARD_TOTAL"], "2");
    a.phase(&on_a, "running").await?;
    b.phase(&on_b, "running").await?;
    let path = format!("/v1/groups/{id}");
    wait_view(client, &path, "running").await?;
    a.result(result(&on_a, "succeeded")).await?;
    b.result(result(&on_b, "failed")).await?;
    let done = wait_view(client, &path, "failed").await?;
    assert_eq!(done["counts"], json!({"succeeded":1,"failed":1}));
    assert_eq!(done["nodes"], json!(["a", "b"]));
    assert!(!done["finishedAt"].is_null());
    let failed = done["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["state"] == "failed")
        .context("failed shard")?;
    let retried = client.post(&format!("{path}/retry"), json!({})).await?;
    assert_eq!(retried["state"], "running");
    let assigned = tokio::select! {
        assignment=a.next("job.assign")=>assignment?,
        assignment=b.next("job.assign")=>assignment?,
    };
    assert_eq!(
        assigned["spec"]["command"][2],
        format!("run --shard={}/2", failed["shardIndex"])
    );
    let again = client.get(&path).await?;
    let attempt = again["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["shardIndex"] == failed["shardIndex"])
        .context("retried shard")?;
    assert_eq!(attempt["attempt"], 2);
    assert_eq!(attempt["retryOf"], failed["id"]);
    assert!(
        client.get("/v1/groups").await?["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group["id"] == id)
    );
    Ok(())
}
