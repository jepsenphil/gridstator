//! Linux CLI coordination through a simulated systemctl boundary and real private TLS servers.
//! These tests never install, start or modify an OS service.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use gridstator::{
    client::{Client, Config},
    state,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::process::Child;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn command(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("GRIDSTATOR_CONFIG", root.join("client.json"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("tools").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("LOCAL_TEST_ROOT", root);
    for key in [
        "GRIDSTATOR_URL",
        "GRIDSTATOR_TOKEN",
        "GRIDSTATOR_CA_FILE",
        "GRIDSTATOR_CONTROLLER_DATA",
        "GRIDSTATOR_AGENT_DATA",
        "GRIDSTATOR_SUPERVISED",
        "GRIDSTATOR_INSTALL_ROOT",
    ] {
        command.env_remove(key);
    }
    command
}
fn service_properties(root: &Path, active: bool) {
    fs::write(root.join("properties"),format!("LoadState=loaded\nActiveState={}\nExecStart={{ argv[]={:?} controller start --data-dir {:?} ; ignore_errors=no ; }}\nEnvironment=\n",if active {"active"}else{"inactive"},env!("CARGO_BIN_EXE_gridstator"),root.join("controller"))).unwrap();
}
async fn stop(child: &mut Option<Child>) {
    if let Some(mut process) = child.take() {
        if let Some(id) = process.id() {
            unsafe {
                libc::kill(id as i32, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(5), process.wait())
            .await
            .is_err()
        {
            let _ = process.kill().await;
            let _ = process.wait().await;
        }
    }
}
async fn launch(root: &Path) -> Result<Child> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("controller.log"))?;
    let mut process = tokio::process::Command::from(command(root));
    process
        .args(["controller", "start", "--data-dir"])
        .arg(root.join("controller"))
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let mut child = process.spawn()?;
    let file: Value = state::read_json(&root.join("controller/controller.json"))?;
    let mut config: Config = serde_json::from_slice(&fs::read(root.join("client.json"))?)?;
    config.url = format!(
        "https://{}:{}",
        file["host"].as_str().unwrap(),
        file["port"].as_u64().unwrap()
    );
    let client = Client::new(config)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "private controller failed: {status}; {}",
                fs::read_to_string(root.join("controller.log"))?
            );
        }
        if client.get("/v1/health").await.is_ok() {
            return Ok(child);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "private controller readiness timed out"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
struct Fixture {
    root: Arc<tempfile::TempDir>,
    stop: CancellationToken,
    manager: Option<tokio::task::JoinHandle<()>>,
    client: Client,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Fixture {
    async fn new(active: bool) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let root = Arc::new(tempfile::tempdir()?);
        let directory = root.path().join("controller");
        gridstator::controller::initialize(
            &directory,
            "127.0.0.1",
            port(),
            &[],
            false,
            &root.path().join("client.json"),
        )
        .await?;
        fs::create_dir(root.path().join("tools"))?;
        let script = root.path().join("tools/systemctl");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$LOCAL_TEST_ROOT/trace"
case " $* " in
  *' show '*) /bin/cat "$LOCAL_TEST_ROOT/properties" ;;
  *' restart '*)
    /bin/rm -f "$LOCAL_TEST_ROOT/ack"
    printf restart > "$LOCAL_TEST_ROOT/request"
    while [ ! -f "$LOCAL_TEST_ROOT/ack" ]; do /bin/sleep 0.01; done
    code=$(/bin/cat "$LOCAL_TEST_ROOT/ack")
    exit "$code"
    ;;
  *) exit 98 ;;
esac
"#,
        )?;
        fs::set_permissions(script, fs::Permissions::from_mode(0o755))?;
        let client = Client::new(serde_json::from_slice(&fs::read(
            root.path().join("client.json"),
        )?)?)?;
        let finish = CancellationToken::new();
        let cancel = finish.clone();
        let owned = root.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let manager = tokio::spawn(async move {
            let mut child = if active {
                Some(
                    launch(owned.path())
                        .await
                        .expect("initial private controller"),
                )
            } else {
                None
            };
            service_properties(owned.path(), child.is_some());
            let _ = ready.send(());
            loop {
                tokio::select! {biased;_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(10))=>{}}
                if owned.path().join("request").is_file() {
                    fs::remove_file(owned.path().join("request")).unwrap();
                    stop(&mut child).await;
                    let result = launch(owned.path()).await;
                    let code = if let Ok(process) = result {
                        child = Some(process);
                        0
                    } else {
                        97
                    };
                    service_properties(owned.path(), child.is_some());
                    state::write_atomic(&owned.path().join("ack"), code.to_string().as_bytes())
                        .unwrap();
                }
            }
            stop(&mut child).await;
        });
        started.await?;
        Ok(Self {
            root,
            stop: finish,
            manager: Some(manager),
            client,
        })
    }
    fn directory(&self) -> PathBuf {
        self.root.path().join("controller")
    }
    async fn invoke(&self, arguments: Vec<String>) -> Output {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || command(root.path()).args(arguments).output().unwrap())
            .await
            .unwrap()
    }
    async fn local(&self, action: &str, extra: &[String]) -> Output {
        let mut args = vec![
            "controller".into(),
            "local".into(),
            action.into(),
            "--data-dir".into(),
            self.directory().to_string_lossy().into_owned(),
            "--config".into(),
            self.root
                .path()
                .join("client.json")
                .to_string_lossy()
                .into_owned(),
        ];
        args.extend_from_slice(extra);
        self.invoke(args).await
    }
    async fn configure(&self, host: &str, port: u16, url: &str) -> Output {
        self.local(
            "configure",
            &[
                "--host".into(),
                host.into(),
                "--port".into(),
                port.to_string(),
                "--url".into(),
                url.into(),
            ],
        )
        .await
    }
    async fn close(mut self) {
        self.stop.cancel();
        self.manager.take().unwrap().await.unwrap();
    }
    async fn worker(
        &self,
        name: &str,
        endpoint: bool,
    ) -> Result<(
        String,
        WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    )> {
        let token = self
            .client
            .post("/v1/tokens", json!({"kind":"join","name":"scripted"}))
            .await?;
        let mut config = self.client.config.clone();
        config.token = token["token"].as_str().unwrap().into();
        let pairing = Client::new(config)?;
        let mut info = json!({"hostname":name,"platform":"linux","arch":"x64","osRelease":"test","cpus":8,"memoryTotalMb":16384,"agentVersion":gridstator::VERSION,"docker":{"available":true},"buildx":{"available":false},"act":{"available":false},"executors":["command"]});
        if endpoint {
            info["controllerUrl"] = json!(self.client.config.url);
        }
        let registered = pairing
            .post("/v1/agents/register", json!({"name":name,"info":info}))
            .await?;
        let mut request = format!(
            "{}/v1/agent/ws",
            self.client.config.url.replacen("https:", "wss:", 1)
        )
        .into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", registered["credential"].as_str().unwrap()).parse()?,
        );
        let (mut socket, _) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(
                gridstator::pki::websocket_tls(&self.client.config.ca_pem)?,
            )),
        )
        .await?;
        socket.send(Message::Text(json!({"type":"hello","protocolVersion":1,"info":info,"activeJobs":[],"results":[]}).to_string().into())).await?;
        let welcome = socket.next().await.context("missing welcome")??;
        ensure!(welcome.to_text()?.contains("welcome"), "unexpected welcome");
        Ok((registered["nodeId"].as_str().unwrap().into(), socket))
    }
}
#[tokio::test]
async fn source_local_010_active_embedded_job_blocks_restart_and_restores_scheduling() -> Result<()>
{
    let fixture = Fixture::new(true).await?;
    let (id, mut worker) = fixture.worker("embedded", false).await?;
    state::write_json(
        &fixture.directory().join("embedded-agent/agent.json"),
        &json!({"nodeId":id}),
    )?;
    let job=fixture.client.post("/v1/jobs",json!({"spec":{"kind":"command","command":["true"],"image":"alpine","resources":{"cpus":1,"memoryMb":512}}})).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = worker.next().await.context("assignment socket closed")??;
            let value: Value = serde_json::from_str(value.to_text()?)?;
            if value["type"] == "job.assign" {
                break Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    let output = fixture.local("service", &["restart".into()]).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Finish the 1 running job"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !fs::read_to_string(fixture.root.path().join("trace"))?
            .lines()
            .any(|line| line.split_whitespace().any(|word| word == "restart"))
    );
    assert!(
        fixture
            .client
            .get(&format!("/v1/jobs/{}", job["id"].as_str().unwrap()))
            .await?["cancelRequestedAt"]
            .is_null()
    );
    assert_eq!(
        fixture.client.get(&format!("/v1/nodes/{id}")).await?["settings"]["schedulingMode"],
        "enabled"
    );
    worker.close(None).await?;
    fixture.close().await;
    Ok(())
}
#[tokio::test]
async fn source_local_011_retains_old_listener_for_offline_or_unadvertised_workers() -> Result<()> {
    let fixture = Fixture::new(true).await?;
    let (_, mut worker) = fixture.worker("offline", false).await?;
    worker.close(None).await?;
    let old = fixture.client.config.url.clone();
    let target = port();
    let url = format!("https://127.0.0.1:{target}");
    let output = fixture.configure("127.0.0.1", target, &url).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let view: Value = serde_json::from_slice(&output.stdout)?;
    assert!(
        view["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|alias| alias["port"] == url::Url::parse(&old).unwrap().port().unwrap())
    );
    assert_eq!(
        fixture.client.get("/v1/cluster").await?["clusterId"],
        view["clusterId"]
    );
    let saved: Value = state::read_json(&fixture.root.path().join("client.json"))?;
    assert_eq!(saved["url"], url);
    fixture.close().await;
    Ok(())
}
#[tokio::test]
async fn source_local_012_apply_and_restart_starts_a_previously_stopped_controller() -> Result<()> {
    let fixture = Fixture::new(false).await?;
    let target = port();
    let output = fixture
        .configure("127.0.0.1", target, &format!("https://127.0.0.1:{target}"))
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let view: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(view["service"]["state"], "active");
    assert_eq!(view["port"], target);
    fixture.close().await;
    Ok(())
}
#[tokio::test]
async fn source_local_013_requires_fresh_worker_connection_even_at_unchanged_public_endpoint()
-> Result<()> {
    let fixture = Fixture::new(true).await?;
    let (_, mut worker) = fixture.worker("does-not-reconnect", true).await?;
    let previous: Value = state::read_json(&fixture.directory().join("controller.json"))?;
    let port = previous["port"].as_u64().unwrap() as u16;
    let output = fixture
        .configure("127.0.0.2", port, &fixture.client.config.url)
        .await;
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("workers did not reconnect"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        state::read_json(&fixture.directory().join("controller.json"))?,
        previous
    );
    assert_eq!(fixture.client.get("/v1/health").await?["ok"], true);
    let _ = worker.close(None).await;
    fixture.close().await;
    Ok(())
}
#[tokio::test]
async fn source_local_014_busy_new_listener_restores_previous_configuration_and_service()
-> Result<()> {
    let fixture = Fixture::new(true).await?;
    let before = fs::read(fixture.directory().join("controller.json"))?;
    let busy = std::net::TcpListener::bind("127.0.0.1:0")?;
    let target = busy.local_addr()?.port();
    let output = fixture
        .configure("127.0.0.1", target, &format!("https://127.0.0.1:{target}"))
        .await;
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("previous configuration was restored"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(fixture.directory().join("controller.json"))?,
        before
    );
    assert_eq!(fixture.client.get("/v1/health").await?["ok"], true);
    fixture.close().await;
    Ok(())
}

#[tokio::test]
async fn source_self_update_004_offline_cli_credential_authenticates_on_running_controller()
-> Result<()> {
    let fixture = Fixture::new(true).await?;
    let output = fixture
        .invoke(vec![
            "controller".into(),
            "token".into(),
            "create".into(),
            "--data-dir".into(),
            fixture.directory().to_string_lossy().into_owned(),
            "--name".into(),
            "recovered".into(),
            "--role".into(),
            "admin".into(),
        ])
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    assert!(token.starts_with("gsc_"));
    let mut config = fixture.client.config.clone();
    config.token = token;
    let recovered = Client::new(config)?;
    assert_eq!(
        recovered.get("/v1/cluster").await?["whoami"],
        json!({"name":"recovered","role":"admin"})
    );
    assert!(fixture.client.config.url.starts_with("https://127.0.0.1:"));
    fs::remove_file(fixture.root.path().join("client.json"))?;
    let output = fixture
        .invoke(vec![
            "controller".into(),
            "token".into(),
            "create".into(),
            "--data-dir".into(),
            fixture.directory().to_string_lossy().into_owned(),
            "--name".into(),
            "recovered".into(),
            "--role".into(),
            "admin".into(),
            "--login".into(),
        ])
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let saved: Config =
        serde_json::from_slice(&fs::read(fixture.root.path().join("client.json"))?)?;
    assert!(saved.url.starts_with("https://127.0.0.1:"));
    assert_eq!(saved.ca_pem, fixture.client.config.ca_pem);
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&saved.token));
    assert_eq!(
        Client::new(saved)?.get("/v1/cluster").await?["whoami"],
        json!({"name":"recovered","role":"admin"})
    );
    fixture.close().await;
    Ok(())
}
