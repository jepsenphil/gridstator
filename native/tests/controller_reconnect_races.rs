//! Deterministic same-socket inventory races found by the real two-worker Docker scenario.
//! A refresh inventory is captured before the worker reads its next assignment/cancel frame.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use gridstator::{
    client::{Client, Config},
    controller::{self, ControllerTiming},
    db, pki, state,
};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

const WAIT: Duration = Duration::from_secs(5);
struct Fixture {
    _directory: tempfile::TempDir,
    server: tokio::task::JoinHandle<Result<()>>,
    client: Client,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn boot() -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir()?;
        let data = directory.path().join("controller");
        let login = directory.path().join("client.json");
        controller::initialize(&data, "127.0.0.1", 0, &[], false, &login).await?;
        db::Db::open(&data.join("controller.db"))?
            .write(|connection| {
                db::put(
                    connection,
                    "settings",
                    "cluster",
                    &json!({"autoUpdate":"off","toolchain":{"act":"off"}}),
                )
            })
            .await?;
        let mut config: Config = serde_json::from_slice(&std::fs::read(login)?)?;
        let timing = ControllerTiming {
            schedule_interval: Duration::from_millis(10),
            cancel_timeout_ms: 30_000,
            assign_timeout_ms: 30_000,
            retention_interval: Duration::from_secs(3600),
            ..ControllerTiming::default()
        };
        let server = tokio::spawn(controller::start_with_timing(
            data.clone(),
            None,
            Some(0),
            timing,
        ));
        let client = tokio::time::timeout(WAIT, async {
            loop {
                if server.is_finished() {
                    bail!("private controller stopped before readiness");
                }
                if let Ok(runtime) = state::read_json(&data.join("runtime.json"))
                    && let Some(port) = runtime["port"].as_u64()
                {
                    config.url = format!("https://127.0.0.1:{port}");
                    let client = Client::new(config.clone())?;
                    if client.get("/v1/health").await.is_ok() {
                        return Ok::<_, anyhow::Error>(client);
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(Self {
            _directory: directory,
            server,
            client,
        })
    }
    async fn submit(&self) -> Result<Value> {
        self.client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine","command":["true"],"resources":{"cpus":1,"memoryMb":512}}})).await
    }
    async fn job(&self, id: &Value, expected: &str) -> Result<Value> {
        let path = format!("/v1/jobs/{}", id.as_str().context("job ID")?);
        tokio::time::timeout(WAIT, async {
            loop {
                let job = self.client.get(&path).await?;
                if job["state"] == expected {
                    return Ok::<_, anyhow::Error>(job);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .with_context(|| format!("waiting for job state {expected}"))?
    }
    async fn assert_allocation(&self, registration: &Value, job: &Value) -> Result<()> {
        let node = self
            .client
            .get(&format!(
                "/v1/nodes/{}",
                registration["nodeId"].as_str().unwrap()
            ))
            .await?;
        assert_eq!(node["activeJobs"], json!([job["id"]]));
        assert_eq!(node["allocated"]["cpus"].as_f64(), Some(1.0));
        assert_eq!(node["allocated"]["memoryMb"], 512);
        Ok(())
    }
}
fn info() -> Value {
    json!({"hostname":"refresh-race","platform":"linux","arch":"x64","osRelease":"test","cpus":8,"memoryTotalMb":16384,"agentVersion":gridstator::VERSION,"docker":{"available":true},"buildx":{"available":true},"act":{"available":false},"executors":["command"]})
}
struct Agent {
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    pending: VecDeque<Value>,
    registration: Value,
}
impl Agent {
    async fn connect(client: &Client) -> Result<Self> {
        let token = client
            .post(
                "/v1/tokens",
                json!({"kind":"join","name":"race-pairing","maxUses":1}),
            )
            .await?;
        let mut config = client.config.clone();
        config.token = token["token"].as_str().context("join token")?.into();
        let registration = Client::new(config)?
            .post(
                "/v1/agents/register",
                json!({"name":"refresh-worker","info":info()}),
            )
            .await?;
        let mut request = format!(
            "{}/v1/agent/ws",
            client.config.url.replacen("https:", "wss:", 1)
        )
        .into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!(
                "Bearer {}",
                registration["credential"]
                    .as_str()
                    .context("agent credential")?
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
            registration,
        };
        agent.empty_refresh().await?;
        agent.next("welcome").await?;
        Ok(agent)
    }
    async fn send(&mut self, message: Value) -> Result<()> {
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await?;
        Ok(())
    }
    async fn empty_refresh(&mut self) -> Result<()> {
        self.send(
            json!({"type":"hello","protocolVersion":1,"info":info(),"activeJobs":[],"results":[]}),
        )
        .await
    }
    async fn next(&mut self, expected: &str) -> Result<Value> {
        tokio::time::timeout(WAIT, async {
            if let Some(index) = self
                .pending
                .iter()
                .position(|value| value["type"] == expected)
            {
                return Ok(self.pending.remove(index).unwrap());
            }
            loop {
                let frame = self.socket.next().await.context("agent socket closed")??;
                if let Message::Text(text) = frame {
                    let value: Value = serde_json::from_str(&text)?;
                    if value["type"] == expected {
                        return Ok(value);
                    }
                    self.pending.push_back(value);
                } else if frame.is_close() {
                    bail!("agent socket closed while waiting for {expected}");
                }
            }
        })
        .await
        .with_context(|| format!("waiting for {expected}"))?
    }
    async fn running(&mut self, assignment: &Value) -> Result<()> {
        for phase in ["starting", "running"] {
            self.send(json!({"type":"job.state","jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"phase":phase,"ts":state::now()})).await?;
        }
        Ok(())
    }
    async fn finish(&mut self, assignment: &Value, outcome: &str) -> Result<()> {
        self.send(json!({"type":"job.result","result":{"jobId":assignment["jobId"],"assignmentId":assignment["assignmentId"],"outcome":outcome,"exitCode":if outcome=="succeeded"{0}else{143},"error":null,"finishedAt":state::now(),"logSeq":0}})).await?;
        assert_eq!(self.next("job.ack").await?["jobId"], assignment["jobId"]);
        Ok(())
    }
}

#[tokio::test]
async fn refresh_before_reading_assignment_keeps_identity_resources_and_success() -> Result<()> {
    let fixture = Fixture::boot().await?;
    let mut agent = Agent::connect(&fixture.client).await?;
    let submitted = fixture.submit().await?;
    let assigned = fixture.job(&submitted["id"], "assigned").await?;
    // The assignment is in the server->worker stream, while this old inventory travels back.
    agent.empty_refresh().await?;
    agent.next("welcome").await?;
    let after = fixture
        .client
        .get(&format!("/v1/jobs/{}", submitted["id"].as_str().unwrap()))
        .await?;
    assert_eq!(
        after["state"], "assigned",
        "same-socket refresh requeued in-flight work"
    );
    assert_eq!(
        after["assignmentId"], assigned["assignmentId"],
        "same-socket refresh replaced assignment"
    );
    fixture
        .assert_allocation(&agent.registration, &assigned)
        .await?;
    let assignment = agent.next("job.assign").await?;
    assert_eq!(assignment["assignmentId"], assigned["assignmentId"]);
    agent.running(&assignment).await?;
    fixture.job(&submitted["id"], "running").await?;
    agent.finish(&assignment, "succeeded").await?;
    fixture.job(&submitted["id"], "succeeded").await?;
    Ok(())
}

#[tokio::test]
async fn empty_refresh_keeps_same_generation_running_work() -> Result<()> {
    let fixture = Fixture::boot().await?;
    let mut agent = Agent::connect(&fixture.client).await?;
    let submitted = fixture.submit().await?;
    let assignment = agent.next("job.assign").await?;
    agent.running(&assignment).await?;
    fixture.job(&submitted["id"], "running").await?;
    agent.empty_refresh().await?;
    agent.next("welcome").await?;
    let after = fixture
        .client
        .get(&format!("/v1/jobs/{}", submitted["id"].as_str().unwrap()))
        .await?;
    assert_eq!(
        after["state"], "running",
        "same-socket refresh lost running work"
    );
    assert_eq!(after["assignmentId"], assignment["assignmentId"]);
    fixture
        .assert_allocation(&agent.registration, &after)
        .await?;
    agent.finish(&assignment, "succeeded").await?;
    fixture.job(&submitted["id"], "succeeded").await?;
    Ok(())
}

#[tokio::test]
async fn empty_refresh_keeps_running_work_and_pending_operator_cancellation() -> Result<()> {
    let fixture = Fixture::boot().await?;
    let mut agent = Agent::connect(&fixture.client).await?;
    let submitted = fixture.submit().await?;
    let assignment = agent.next("job.assign").await?;
    agent.running(&assignment).await?;
    fixture.job(&submitted["id"], "running").await?;
    let cancelled = fixture
        .client
        .post(
            &format!("/v1/jobs/{}/cancel", submitted["id"].as_str().unwrap()),
            json!({}),
        )
        .await?;
    assert!(!cancelled["cancelRequestedAt"].is_null());
    agent.empty_refresh().await?;
    agent.next("welcome").await?;
    let after = fixture
        .client
        .get(&format!("/v1/jobs/{}", submitted["id"].as_str().unwrap()))
        .await?;
    assert_eq!(
        after["state"], "running",
        "same-socket refresh prematurely finalized a pending cancellation"
    );
    assert_eq!(after["assignmentId"], assignment["assignmentId"]);
    assert_eq!(after["cancelRequestedAt"], cancelled["cancelRequestedAt"]);
    fixture
        .assert_allocation(&agent.registration, &after)
        .await?;
    assert_eq!(agent.next("job.cancel").await?["jobId"], submitted["id"]);
    agent.finish(&assignment, "cancelled").await?;
    fixture.job(&submitted["id"], "cancelled").await?;
    Ok(())
}
