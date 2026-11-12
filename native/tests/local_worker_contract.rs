//! Actual private bridge + worker processes; no OS service or production account state is mutated.
#![cfg(unix)]
use anyhow::{Context, Result, ensure};
use gridstator::{
    client::{Client, Config},
    controller, state,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

struct Child(tokio::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

fn command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .env_remove("GRIDSTATOR_SUPERVISED")
        .env_remove("GRIDSTATOR_INSTALL_ROOT")
        .env_remove("GRIDSTATOR_INSTALL_VERSION")
        .env_remove("GRIDSTATOR_URL")
        .env_remove("GRIDSTATOR_TOKEN")
        .env_remove("GRIDSTATOR_CA_FILE")
        .env("GRIDSTATOR_DOCKER", "/bin/false")
        .env("GRIDSTATOR_ACT", "/bin/false")
        .kill_on_drop(true);
    command
}

async fn bridge(request: Value) -> Result<Value> {
    let mut child = command()
        .arg("bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request)?)
        .await?;
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await??;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn wait_worker(dir: &Path) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let value =
                bridge(json!({"version":1,"operation":"worker","action":"status","dataDir":dir}))
                    .await?;
            if value["workers"][0]["running"] == true {
                return Ok::<_, anyhow::Error>(value["workers"][0].clone());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await?
}

async fn offline_worker(root: &Path) -> Result<PathBuf> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let controller_dir = root.join("authority");
    let init = controller::initialize(
        &controller_dir,
        "127.0.0.1",
        1,
        &[],
        false,
        &root.join("client.json"),
    )
    .await?;
    let dir = root.join("agent");
    state::private_dir(&dir)?;
    state::write_json(
        &dir.join("agent.json"),
        &json!({"version":2,"nodeId":"local-worker-fixture","name":"offline worker","clusterId":init["clusterId"],"controllerUrl":"https://127.0.0.1:1","caPem":std::fs::read_to_string(controller_dir.join("pki/ca.pem"))?,"caFingerprint":init["caFingerprint"],"credential":"gsa_fixture_private"}),
    )?;
    Ok(dir)
}

fn run_worker(dir: &Path) -> Result<Child> {
    Ok(Child(
        command()
            .args(["agent", "start", "--data-dir"])
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ))
}

#[tokio::test]
async fn private_control_works_offline_and_preserves_pause_and_budget_across_restart() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let mut actor = run_worker(&dir)?;
    let initial = wait_worker(&dir).await?;
    let request = |action: &str| json!({"version":1,"operation":"worker","action":action,"dataDir":dir,"nodeId":initial["nodeId"],"caFingerprint":initial["caFingerprint"]});
    let paused = bridge(request("pause")).await?;
    assert_eq!(paused["workers"][0]["paused"], true);
    let mut budget = request("setBudget");
    budget["budget"] = json!({"cpus":1.5,"memoryMb":768,"maxConcurrency":1});
    let updated = bridge(budget).await?;
    assert_eq!(updated["workers"][0]["budget"]["cpus"], 1.5);
    assert!(!serde_json::to_string(&updated)?.contains("gsa_fixture_private"));
    let mut stale = request("resume");
    stale["nodeId"] = json!("different-registration");
    assert!(
        bridge(stale)
            .await
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert!(
        bridge(request("forceStop"))
            .await
            .unwrap_err()
            .to_string()
            .contains("confirmation")
    );
    let stopped = bridge(request("stop")).await?;
    assert_eq!(stopped["workers"][0]["draining"], true);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), actor.0.wait())
            .await??
            .success()
    );
    let saved = bridge(request("status")).await?;
    assert_eq!(saved["workers"][0]["running"], false);
    assert_eq!(saved["workers"][0]["paused"], true);
    let mut again = run_worker(&dir)?;
    let after = wait_worker(&dir).await?;
    assert_eq!(after["paused"], true);
    assert_eq!(after["budget"]["memoryMb"], 768);
    bridge(request("stop")).await?;
    tokio::time::timeout(Duration::from_secs(5), again.0.wait()).await??;
    Ok(())
}

#[tokio::test]
async fn repeated_private_join_reuses_a_running_worker_without_installing_or_changing_intent()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let mut actor = run_worker(&dir)?;
    let initial = wait_worker(&dir).await?;
    let paused =
        bridge(json!({"version":1,"operation":"worker","action":"pause","dataDir":dir})).await?;
    let saved = std::fs::read(dir.join("agent.json"))?;
    let reused = bridge(json!({"version":1,"operation":"worker","action":"join","dataDir":dir,"url":initial["controllerUrl"],"fingerprint":initial["caFingerprint"],"token":"gsj_unused_private","startAtSignIn":false})).await?;
    assert_eq!(reused["workers"][0]["nodeId"], initial["nodeId"]);
    assert_eq!(reused["workers"][0]["running"], true);
    assert_eq!(reused["workers"][0]["paused"], true);
    assert_eq!(
        reused["workers"][0]["generation"],
        paused["workers"][0]["generation"]
    );
    assert_eq!(std::fs::read(dir.join("agent.json"))?, saved);
    assert!(actor.0.try_wait()?.is_none());
    bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":dir})).await?;
    tokio::time::timeout(Duration::from_secs(5), actor.0.wait()).await??;
    Ok(())
}

#[tokio::test]
async fn embedded_classification_requires_an_owned_matching_parent_controller() -> Result<()> {
    let root = tempfile::tempdir()?;
    let original = offline_worker(root.path()).await?;
    let standalone = root.path().join("embedded-agent");
    std::fs::rename(&original, &standalone)?;
    let status =
        |dir: &Path| json!({"version":1,"operation":"worker","action":"status","dataDir":dir});
    assert_eq!(
        bridge(status(&standalone)).await?["workers"][0]["embedded"],
        false
    );
    let genuine = root.path().join("authority/embedded-agent");
    std::fs::rename(&standalone, &genuine)?;
    assert_eq!(
        bridge(status(&genuine)).await?["workers"][0]["embedded"],
        true
    );
    let config_path = genuine.join("agent.json");
    state::update_json(&config_path, |value| {
        value.as_object_mut().unwrap().remove("clusterId");
        Ok(())
    })?;
    assert_eq!(
        bridge(status(&genuine)).await?["workers"][0]["embedded"],
        true
    );
    state::update_json(&config_path, |value| {
        value["clusterId"] = json!("different-cluster");
        Ok(())
    })?;
    assert_eq!(
        bridge(status(&genuine)).await?["workers"][0]["embedded"],
        false
    );
    Ok(())
}

#[tokio::test]
async fn verbose_local_logs_are_byte_bounded_and_continue_from_the_last_returned_sequence()
-> Result<()> {
    use std::io::Write;
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let job = dir.join("jobs/JOB-VERBOSE");
    state::private_dir(&job)?;
    let mut output = std::fs::File::create(job.join("logs.ndjson"))?;
    let data = format!("{}[redacted]", "\\\"\n".repeat(20_000));
    for seq in 1..=30 {
        serde_json::to_writer(
            &mut output,
            &json!({"s":seq,"t":state::now(),"o":"stdout","d":data}),
        )?;
        output.write_all(b"\n")?;
    }
    drop(output);
    let mut after = 0;
    let mut total = 0;
    loop {
        let response = bridge(json!({"version":1,"operation":"worker","action":"logs","dataDir":dir,"jobId":"JOB-VERBOSE","afterSeq":after,"limit":1000})).await?;
        assert!(serde_json::to_vec(&response)?.len() < 600 * 1024);
        let entries = response["logs"]["entries"].as_array().unwrap();
        if entries.is_empty() {
            break;
        }
        assert_eq!(entries[0]["s"], after + 1);
        total += entries.len();
        after = response["logs"]["nextSeq"].as_u64().unwrap();
    }
    assert_eq!(total, 30);
    assert_eq!(after, 30);
    Ok(())
}

#[tokio::test]
async fn graceful_local_stop_does_not_wait_for_offline_result_acknowledgements() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let job = dir.join("jobs").join("JOB-UNACKED");
    state::private_dir(&job)?;
    state::write_json(
        &job.join("assignment.json"),
        &json!({"jobId":"JOB-UNACKED","assignmentId":"ASSIGNMENT","spec":{"name":"offline completed"}}),
    )?;
    state::write_json(
        &job.join("result.json"),
        &json!({"jobId":"JOB-UNACKED","assignmentId":"ASSIGNMENT","outcome":"succeeded","logSeq":0,"finishedAt":state::now()}),
    )?;
    let mut actor = run_worker(&dir)?;
    wait_worker(&dir).await?;
    bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":dir})).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), actor.0.wait())
            .await??
            .success()
    );
    assert_eq!(
        state::read_json(&job.join("result.json"))?["outcome"],
        "succeeded"
    );
    Ok(())
}

#[tokio::test]
async fn embedded_worker_stop_and_start_leave_the_controller_alive() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = tempfile::tempdir()?;
    let dir = root.path().join("controller");
    let init = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &root.path().join("client.json"),
    )
    .await?;
    let mut actor = Child(
        command()
            .args(["controller", "start", "--with-agent", "--data-dir"])
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let worker_dir = dir.join("embedded-agent");
    tokio::time::timeout(Duration::from_secs(15), async {
        while !worker_dir.join("agent.json").exists() {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await?;
    let before = wait_worker(&worker_dir).await?;
    let runtime = state::read_json(&dir.join("runtime.json"))?;
    let client = Client::new(Config {
        version: 1,
        url: format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap()),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: init["caFingerprint"].as_str().unwrap().into(),
        token: init["token"].as_str().unwrap().into(),
        token_name: None,
    })?;
    bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":worker_dir})).await?;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = bridge(
                json!({"version":1,"operation":"worker","action":"status","dataDir":worker_dir}),
            )
            .await?;
            if status["workers"][0]["running"] == false {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await??;
    assert!(actor.0.try_wait()?.is_none());
    assert_eq!(client.get("/v1/health").await?["ok"], true);
    assert_eq!(
        state::read_json(&dir.join("runtime.json"))?["pid"],
        runtime["pid"]
    );
    let restarted =
        bridge(json!({"version":1,"operation":"worker","action":"start","dataDir":worker_dir}))
            .await?;
    assert_eq!(restarted["workers"][0]["running"], true);
    assert_eq!(restarted["workers"][0]["nodeId"], before["nodeId"]);
    assert_eq!(client.get("/v1/health").await?["ok"], true);
    actor.0.start_kill()?;
    let _ = actor.0.wait().await;
    Ok(())
}

#[tokio::test]
async fn symlink_registration_and_job_path_traversal_are_rejected() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let foreign = root.path().join("alias");
    std::os::unix::fs::symlink(&dir, &foreign)?;
    let error =
        bridge(json!({"version":1,"operation":"worker","action":"status","dataDir":foreign}))
            .await
            .unwrap_err();
    assert!(error.to_string().contains("symlink"));
    let error=bridge(json!({"version":1,"operation":"worker","action":"logs","dataDir":dir,"jobId":"../client.json"})).await.unwrap_err();
    assert!(error.to_string().contains("job ID"));
    Ok(())
}

// This synchronization fixture intercepts bind(2) through LD_PRELOAD, which cannot affect
// statically linked musl release binaries. Linux GNU CI exercises the race on a dynamic binary.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[tokio::test]
async fn acknowledged_pause_during_socket_bind_is_visible_to_the_starting_worker() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = offline_worker(root.path()).await?;
    let source = root.path().join("bind-gate.c");
    let shim = root.path().join("bind-gate.so");
    std::fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
int bind(int fd,const struct sockaddr *addr,socklen_t len){
  static int (*original)(int,const struct sockaddr*,socklen_t);
  if(!original)original=dlsym(RTLD_NEXT,"bind");
  const char *gate=getenv("GST_WORKER_BIND_GATE");
  if(gate&&addr&&addr->sa_family==AF_UNIX&&strstr(((const struct sockaddr_un*)addr)->sun_path,"worker-")){
    char ready[4096],release[4096];snprintf(ready,sizeof ready,"%s/ready",gate);snprintf(release,sizeof release,"%s/release",gate);
    FILE*f=fopen(ready,"w");if(f)fclose(f);while(access(release,F_OK)!=0)usleep(10000);
  }
  return original(fd,addr,len);
}
"#,
    )?;
    let compile = std::process::Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&shim)
        .arg("-ldl")
        .output()?;
    ensure!(
        compile.status.success(),
        "bind-gate fixture compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let mut actor = Child(
        command()
            .args(["agent", "start", "--data-dir"])
            .arg(&dir)
            .env("LD_PRELOAD", &shim)
            .env("GST_WORKER_BIND_GATE", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !root.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let selected = dir.clone();
    let pause = tokio::spawn(async move {
        bridge(json!({"version":1,"operation":"worker","action":"pause","dataDir":selected})).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !pause.is_finished(),
        "Pause must not acknowledge an offline write while an actor has loaded its policy"
    );
    assert!(!dir.join("local-policy.json").exists());
    std::fs::write(root.path().join("release"), "")?;
    let acknowledged = pause.await??;
    assert_eq!(acknowledged["workers"][0]["paused"], true);
    let live = wait_worker(&dir).await?;
    assert_eq!(live["paused"], true);
    assert_eq!(live["generation"], acknowledged["workers"][0]["generation"]);
    bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":dir})).await?;
    tokio::time::timeout(Duration::from_secs(5), actor.0.wait()).await??;
    Ok(())
}

async fn controller_fixture() -> Result<(tempfile::TempDir, Child, Client)> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let root = tempfile::tempdir()?;
    let dir = root.path().join("controller");
    let init = controller::initialize(
        &dir,
        "127.0.0.1",
        0,
        &[],
        false,
        &root.path().join("client.json"),
    )
    .await?;
    let actor = Child(
        command()
            .args(["controller", "start", "--data-dir"])
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let runtime = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(value) = state::read_json(&dir.join("runtime.json")) {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await?;
    let client = Client::new(Config {
        version: 1,
        url: format!("https://127.0.0.1:{}", runtime["port"].as_u64().unwrap()),
        ca_pem: std::fs::read_to_string(dir.join("pki/ca.pem"))?,
        ca_fingerprint: init["caFingerprint"].as_str().unwrap().into(),
        token: init["token"].as_str().unwrap().into(),
        token_name: None,
    })?;
    Ok((root, actor, client))
}
async fn real_worker(root: &Path, client: &Client) -> Result<(PathBuf, Child, String)> {
    let dir = root.join("agent");
    let token = client
        .post(
            "/v1/tokens",
            json!({"kind":"join","name":"desktop-worker","uses":1}),
        )
        .await?;
    let joined = gridstator::worker::join(
        &dir,
        &client.config.url,
        token["token"].as_str().unwrap(),
        &client.config.ca_fingerprint,
        Some(format!("desktop-{}", state::id())),
        Some(json!({"reserved":{"cpus":0,"memoryMb":0},"maxConcurrency":2})),
    )
    .await?;
    let actor = Child(
        command()
            .args(["agent", "start", "--data-dir"])
            .arg(&dir)
            .env("GRIDSTATOR_DOCKER", "docker")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if wait_worker(&dir).await?["connected"] == true {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await??;
    Ok((dir, actor, joined["nodeId"].as_str().unwrap().into()))
}
async fn job_state(client: &Client, id: &str, wanted: &str) -> Result<Value> {
    let mut latest = Value::Null;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = client.get(&format!("/v1/jobs/{id}")).await?;
            latest = value.clone();
            if value["state"] == wanted {
                return Ok::<_, anyhow::Error>(value);
            }
            ensure!(
                !gridstator::protocol::terminal(value["state"].as_str().unwrap_or("")),
                "unexpected terminal job: {value}"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for {wanted}; last job {latest}"))?
}

#[tokio::test]
#[ignore = "requires real Docker; exercises 95-second graceful drain beyond service timeout"]
async fn local_pause_and_long_drain_preserve_running_work_and_replay_masked_results() -> Result<()>
{
    let (root, _controller, client) = controller_fixture().await?;
    let (dir, mut actor, node) = real_worker(root.path(), &client)
        .await
        .context("real worker connection")?;
    bridge(json!({"version":1,"operation":"worker","action":"setBudget","dataDir":dir,"budget":{"cpus":0.5,"memoryMb":256,"maxConcurrency":1}})).await?;
    let spec = |command: &str| json!({"kind":"command","image":"alpine:3.20","command":["sh","-c",command],"resources":{"cpus":0.1,"memoryMb":128},"target":{"node":node},"timeoutSeconds":180,"secretEnv":{"LOCAL_SECRET":"desktop-private-fixture"}});
    let first = client
        .post(
            "/v1/jobs",
            json!({"spec":spec("echo before; sleep 95; echo $LOCAL_SECRET; echo after")}),
        )
        .await?;
    let id = first["id"].as_str().unwrap();
    job_state(&client, id, "running")
        .await
        .with_context(|| format!("job did not start: {id}"))?;
    bridge(json!({"version":1,"operation":"worker","action":"pause","dataDir":dir})).await?;
    let next = client
        .post("/v1/jobs", json!({"spec":spec("echo next")}))
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        client
            .get(&format!("/v1/jobs/{}", next["id"].as_str().unwrap()))
            .await?["state"],
        "queued"
    );
    let stopping =
        bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":dir})).await?;
    assert_eq!(
        stopping["workers"][0]["activeJobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    tokio::time::sleep(Duration::from_secs(91)).await;
    assert!(
        actor.0.try_wait()?.is_none(),
        "graceful Stop must not impose the 90-second service timeout"
    );
    assert_eq!(client.get("/v1/health").await?["ok"], true);
    assert!(
        tokio::time::timeout(Duration::from_secs(20), actor.0.wait())
            .await??
            .success()
    );
    let after =
        bridge(json!({"version":1,"operation":"worker","action":"status","dataDir":dir})).await?;
    assert!(
        after["workers"][0]["recentJobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["id"] == id && job["phase"] == "succeeded")
    );
    let logs =
        bridge(json!({"version":1,"operation":"worker","action":"logs","dataDir":dir,"jobId":id}))
            .await?;
    let text = serde_json::to_string(&logs)?;
    assert!(!text.contains("desktop-private-fixture"));
    assert!(text.contains("***"));
    let mut again = Child(
        command()
            .args(["agent", "start", "--data-dir"])
            .arg(&dir)
            .env("GRIDSTATOR_DOCKER", "docker")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    wait_worker(&dir).await?;
    job_state(&client, id, "succeeded").await?;
    bridge(json!({"version":1,"operation":"worker","action":"stop","dataDir":dir})).await?;
    tokio::time::timeout(Duration::from_secs(5), again.0.wait()).await??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires real Docker; verifies force-stop records cancellation rather than lost"]
async fn force_stop_cancels_only_the_local_running_job() -> Result<()> {
    let (root, _controller, client) = controller_fixture().await?;
    let (dir, mut actor, node) = real_worker(root.path(), &client)
        .await
        .context("real worker connection")?;
    let job=client.post("/v1/jobs",json!({"spec":{"kind":"command","image":"alpine:3.20","command":["sh","-c","echo ready; sleep 60"],"resources":{"cpus":0.1,"memoryMb":128},"target":{"node":node},"timeoutSeconds":120}})).await?;
    let id = job["id"].as_str().unwrap();
    job_state(&client, id, "running")
        .await
        .with_context(|| format!("job did not start: {id}"))?;
    bridge(json!({"version":1,"operation":"worker","action":"forceStop","dataDir":dir,"confirmed":true})).await?;
    tokio::time::timeout(Duration::from_secs(15), actor.0.wait()).await??;
    let after =
        bridge(json!({"version":1,"operation":"worker","action":"status","dataDir":dir})).await?;
    assert!(
        after["workers"][0]["recentJobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["id"] == id && job["phase"] == "cancelled")
    );
    assert_eq!(client.get("/v1/health").await?["ok"], true);
    Ok(())
}
