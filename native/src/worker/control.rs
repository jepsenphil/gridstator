//! Private same-account live worker control. Policy acknowledgement precedes admission reopening.
use super::{ActiveJobs, Endpoint, Event, policy::PolicyStore};
use crate::local_worker;
use anyhow::{Context, Result, ensure};
use gridstator_local_control_protocol::{Job, WorkerSnapshot};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub(crate) struct Live {
    pub(crate) dir: PathBuf,
    pub(crate) policy: Arc<PolicyStore>,
    active: ActiveJobs,
    endpoint: Arc<Endpoint>,
    pub(crate) connected: AtomicBool,
    pub(crate) draining: AtomicBool,
    pub(crate) force: AtomicBool,
    pub(crate) shutdown: CancellationToken,
    pub(crate) controller: Mutex<Value>,
    pub(crate) info: RwLock<Value>,
    events: tokio::sync::broadcast::Sender<Event>,
}

impl Live {
    pub(super) fn admit(&self, job: super::Active, temporarily_held: bool) -> Result<()> {
        let policy = self.policy.value.lock().unwrap();
        let controller = self.controller.lock().unwrap();
        let mut active = self.active.lock().unwrap();
        ensure!(!self.draining.load(Ordering::Acquire), "worker is stopping");
        ensure!(
            !temporarily_held,
            "worker is checking an update or stopping"
        );
        let jobs = active
            .values()
            .map(|job| job.assignment.clone())
            .collect::<Vec<_>>();
        super::policy::admission(&policy, &controller, &jobs, &job.assignment)?;
        let info = super::policy::advertise(&self.info.read().unwrap(), &policy);
        let (limit_cpu, limit_memory) =
            crate::scheduler::allocatable(&json!({"info":info,"settings":*controller}));
        let (used_cpu, used_memory) = jobs.iter().map(super::policy::request_resources).fold(
            (0.0, 0_u64),
            |(cpu, memory), (next_cpu, next_memory)| {
                (cpu + next_cpu, memory.saturating_add(next_memory))
            },
        );
        let (cpu, memory) = super::policy::request_resources(&job.assignment);
        ensure!(
            used_cpu + cpu <= limit_cpu + 1e-9
                && used_memory.saturating_add(memory) as f64 <= limit_memory,
            "worker effective resource budget is full"
        );
        active.insert(
            job.assignment["jobId"]
                .as_str()
                .context("job ID missing")?
                .to_owned(),
            job,
        );
        Ok(())
    }
    pub(super) fn new(
        dir: PathBuf,
        active: ActiveJobs,
        endpoint: Arc<Endpoint>,
        events: tokio::sync::broadcast::Sender<Event>,
    ) -> Result<Self> {
        Ok(Self {
            policy: Arc::new(PolicyStore::open(&dir)?),
            dir,
            active,
            endpoint,
            events,
            connected: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            force: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
            controller: Mutex::new(json!({})),
            info: RwLock::new(Value::Null),
        })
    }
    pub(crate) fn snapshot(&self) -> Result<WorkerSnapshot> {
        let config = self.endpoint.config();
        let mut snapshot = local_worker::snapshot(&self.dir, &config)?;
        let policy = self.policy.snapshot();
        let info = self.info.read().unwrap().clone();
        snapshot.running = true;
        snapshot.connected = self.connected.load(Ordering::Acquire);
        snapshot.draining = self.draining.load(Ordering::Acquire);
        snapshot.paused = policy.paused;
        snapshot.generation = policy.generation;
        snapshot.budget = policy.budget;
        snapshot.start_at_sign_in = policy.start_at_sign_in;
        snapshot.controller_settings = Some(self.controller.lock().unwrap().clone());
        if info.is_object() {
            let advertised = super::policy::advertise(&info, &self.policy.snapshot());
            let controller = snapshot.controller_settings.as_ref().unwrap();
            let (cpus, memory) =
                crate::scheduler::allocatable(&json!({"info":advertised,"settings":controller}));
            snapshot.effective_budget = Some(gridstator_local_control_protocol::Budget {
                cpus: Some(cpus),
                memory_mb: Some(memory.max(0.0) as u64),
                max_concurrency: Some(
                    snapshot
                        .budget
                        .max_concurrency
                        .unwrap_or(256)
                        .min(controller["maxConcurrency"].as_u64().unwrap_or(256) as u32),
                ),
            });
        }
        let hold = matches!(
            snapshot.controller_settings.as_ref().unwrap()["schedulingMode"].as_str(),
            Some("disabled" | "draining")
        );
        snapshot.state = if snapshot.draining {
            "draining"
        } else if snapshot.paused {
            "paused"
        } else if !snapshot.connected {
            "disconnected"
        } else if info["docker"]["available"] != true {
            "waitingForDocker"
        } else if hold {
            "controllerPaused"
        } else {
            "accepting"
        }
        .into();
        snapshot.active_jobs = self
            .active
            .lock()
            .unwrap()
            .iter()
            .map(|(id, job)| Job {
                id: id.clone(),
                name: job.assignment["spec"]["name"].as_str().unwrap_or(id).into(),
                phase: job.phase.lock().unwrap().clone(),
                started_at: job.assignment["assignedAt"].as_i64(),
            })
            .collect();
        snapshot.info = Some(super::policy::advertise(&info, &self.policy.snapshot()));
        Ok(snapshot)
    }
    pub(crate) fn notify(&self) {
        let _ = self.events.send(Event::LocalControl);
    }
    pub(crate) fn dispatch(&self, request: &Value) -> Result<Value> {
        let config = self.endpoint.config();
        local_worker::validate_identity(&config, request)?;
        let action = request["action"].as_str().unwrap_or("status");
        let mut logs = None;
        match action {
            "list" | "status" | "start" => {}
            "pause" | "resume" | "setBudget" | "setStartup" => {
                self.policy.update(action, request)?;
                self.notify();
            }
            "stop" | "forceStop" => {
                ensure!(
                    action != "forceStop" || request["confirmed"] == true,
                    "Force stop requires explicit confirmation"
                );
                // The policy gate and assignment insertion use the same locking order.
                let _policy = self.policy.value.lock().unwrap();
                local_worker::mark_stopped(&self.dir)?;
                self.draining.store(true, Ordering::Release);
                self.shutdown.cancel();
                if action == "forceStop" {
                    self.force.store(true, Ordering::Release);
                    for job in self.active.lock().unwrap().values() {
                        job.cancel.cancel();
                    }
                }
                self.notify();
            }
            "cancel" => {
                let id = request["jobId"].as_str().context("jobId required")?;
                let active = self.active.lock().unwrap();
                active
                    .get(id)
                    .context("NOT_FOUND: local job is not running")?
                    .cancel
                    .cancel();
            }
            "logs" => logs = Some(local_worker::logs(&self.dir, request)?),
            _ => anyhow::bail!("unknown worker control action"),
        }
        let mut result = json!({"snapshot":self.snapshot()?});
        if let Some(logs) = logs {
            result["logs"] = serde_json::to_value(logs)?;
        }
        Ok(result)
    }
}

pub(crate) struct Server {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    finish: CancellationToken,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.finish.cancel();
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn bind_socket(path: &Path) -> Result<tokio::net::UnixListener> {
    local_worker::prepare_socket_parent(path)?;
    if path.exists() {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() },
            "FORBIDDEN: foreign control socket"
        );
        ensure!(
            std::os::unix::net::UnixStream::connect(path).is_err(),
            "CONFLICT: local control endpoint is already active"
        );
        // A live worker holds agent.lock before reaching this point. Only remove an owned stale
        // socket after the caller has obtained its instance/supervisor lock.
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub(crate) fn serve(live: Arc<Live>, finish: CancellationToken) -> Result<Server> {
    let path = local_worker::socket_path(&live.dir);
    let listener = bind_socket(&path)?;
    let stopped = finish.clone();
    let task = tokio::spawn(async move {
        loop {
            let accepted =
                tokio::select! {_=stopped.cancelled()=>break,accepted=listener.accept()=>accepted};
            let Ok((stream, _)) = accepted else { continue };
            let live = live.clone();
            let stopped = stopped.clone();
            tokio::spawn(async move {
                let operation = async {
                    ensure!(
                        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
                        "FORBIDDEN: local control account mismatch"
                    );
                    let (reader, mut writer) = stream.into_split();
                    let mut input = vec![];
                    tokio::io::BufReader::new(reader)
                        .take(65_537)
                        .read_until(b'\n', &mut input)
                        .await?;
                    ensure!(input.len() <= 65_536, "local control request too large");
                    let request: Value = serde_json::from_slice(&input)?;
                    let result =
                        tokio::task::spawn_blocking(move || live.dispatch(&request)).await?;
                    let response = match result {
                        Ok(value) => json!({"ok":true,"value":value}),
                        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
                    };
                    let mut output = serde_json::to_vec(&response)?;
                    ensure!(
                        output.len() <= 1024 * 1024,
                        "local control response too large"
                    );
                    output.push(b'\n');
                    writer.write_all(&output).await?;
                    Ok::<_, anyhow::Error>(())
                };
                let _ = tokio::select! {_=stopped.cancelled()=>Ok(()),result=tokio::time::timeout(std::time::Duration::from_secs(5),operation)=>result.map_err(anyhow::Error::from).and_then(|result|result)};
            });
        }
    });
    Ok(Server { path, task, finish })
}

pub(crate) struct EmbeddedSupervisor {
    server: Server,
    pub(crate) start: Arc<tokio::sync::Notify>,
}
impl EmbeddedSupervisor {
    pub(crate) fn bind(controller_dir: &Path, finish: CancellationToken) -> Result<Self> {
        let path = local_worker::embedded_socket_path(controller_dir);
        let listener = bind_socket(&path)?;
        let start = Arc::new(tokio::sync::Notify::new());
        let notify = start.clone();
        let stopped = finish.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted =
                    tokio::select! {_=stopped.cancelled()=>break,result=listener.accept()=>result};
                let Ok((stream, _)) = accepted else { continue };
                let notify = notify.clone();
                tokio::spawn(async move {
                    let operation = async {
                        ensure!(
                            stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
                            "FORBIDDEN: local control account mismatch"
                        );
                        let (reader, mut writer) = stream.into_split();
                        let mut input = vec![];
                        tokio::io::BufReader::new(reader)
                            .take(4097)
                            .read_until(b'\n', &mut input)
                            .await?;
                        ensure!(input.len() <= 4096, "request too large");
                        let request: Value = serde_json::from_slice(&input)?;
                        ensure!(request["action"] == "start", "unsupported embedded control");
                        notify.notify_one();
                        writer.write_all(b"{\"ok\":true,\"value\":{}}\n").await?;
                        Ok::<_, anyhow::Error>(())
                    };
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(3), operation).await;
                });
            }
        });
        Ok(Self {
            server: Server { path, task, finish },
            start,
        })
    }
}
impl Drop for EmbeddedSupervisor {
    fn drop(&mut self) {
        self.server.finish.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Client, Config};
    use std::sync::atomic::AtomicU64;

    fn fixture(dir: &Path) -> Result<Arc<Live>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        crate::pki::generate(&dir.join("authority"), "fixture", &[])?;
        let ca = std::fs::read_to_string(dir.join("authority/pki/ca.pem"))?;
        let config = json!({"version":2,"name":"fixture","nodeId":"fixture-node","controllerUrl":"https://localhost:1","caPem":ca,"caFingerprint":crate::pki::fingerprint(&ca)?,"credential":"gsa_private"});
        crate::state::write_json(&dir.join("agent.json"), &config)?;
        let client = Client::new(Config {
            version: 1,
            url: "https://localhost:1".into(),
            ca_pem: ca,
            ca_fingerprint: config["caFingerprint"].as_str().unwrap().into(),
            token: "gsa_private".into(),
            token_name: None,
        })?;
        let endpoint = Arc::new(Endpoint {
            dir: dir.to_owned(),
            config: Mutex::new(config),
            client: RwLock::new(client),
            relocating: tokio::sync::Mutex::new(()),
        });
        let (events, _) = tokio::sync::broadcast::channel(16);
        let live = Arc::new(Live::new(
            dir.to_owned(),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            endpoint,
            events,
        )?);
        *live.info.write().unwrap() =
            json!({"cpus":8,"memoryTotalMb":16384,"docker":{"available":true}});
        *live.controller.lock().unwrap() = json!({"maxConcurrency":256,"reserved":{"cpus":1,"memoryMb":512},"schedulingMode":"enabled"});
        Ok(live)
    }
    fn job(id: &str, cpus: f64, memory: u64) -> super::super::Active {
        super::super::Active {
            assignment: json!({"jobId":id,"assignmentId":id,"spec":{"resources":{"cpus":cpus,"memoryMb":memory}}}),
            cancel: CancellationToken::new(),
            phase: Arc::new(Mutex::new("starting".into())),
            seq: Arc::new(AtomicU64::new(0)),
            pins: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        }
    }
    #[test]
    fn pause_ack_fences_concurrent_assignment_acceptance() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let live = fixture(dir.path())?;
        let begin = Arc::new(std::sync::Barrier::new(5));
        let acknowledged = Arc::new(AtomicBool::new(false));
        let mut threads = vec![];
        for thread in 0..4 {
            let live = live.clone();
            let begin = begin.clone();
            let acknowledged = acknowledged.clone();
            threads.push(std::thread::spawn(move || {
                begin.wait();
                for index in 0..1000 {
                    let after_ack = acknowledged.load(Ordering::Acquire);
                    let result = live.admit(job(&format!("{thread}-{index}"), 0.001, 1), false);
                    assert!(
                        !after_ack || result.is_err(),
                        "assignment admitted after acknowledged Pause"
                    );
                }
            }));
        }
        begin.wait();
        live.dispatch(&json!({"action":"pause"}))?;
        acknowledged.store(true, Ordering::Release);
        let accepted = live.active.lock().unwrap().len();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(live.active.lock().unwrap().len(), accepted);
        assert!(live.policy.snapshot().paused);
        Ok(())
    }
    #[test]
    fn effective_budget_counts_starting_assignments_and_preserves_controller_hold() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let live = fixture(dir.path())?;
        live.dispatch(
            &json!({"action":"setBudget","budget":{"cpus":2.0,"memoryMb":1024,"maxConcurrency":2}}),
        )?;
        live.admit(job("first", 1.0, 512), false)?;
        live.admit(job("second", 1.0, 512), false)?;
        assert!(live.admit(job("third", 0.01, 1), false).is_err());
        let snapshot = live.snapshot()?;
        assert_eq!(snapshot.effective_budget.unwrap().cpus, Some(2.0));
        // Lower limits constrain new admissions without cancelling already accepted starting work.
        live.dispatch(&json!({"action":"setBudget","budget":{"cpus":1.0}}))?;
        assert_eq!(live.active.lock().unwrap().len(), 2);
        live.controller.lock().unwrap()["schedulingMode"] = json!("disabled");
        live.dispatch(&json!({"action":"pause"}))?;
        live.dispatch(&json!({"action":"resume"}))?;
        assert!(live.admit(job("blocked", 0.01, 1), false).is_err());
        Ok(())
    }
    #[test]
    fn routine_hellos_do_not_reapply_cached_controller_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let live = fixture(dir.path())?;
        let config = json!({"settings":{"maxConcurrency":200,"schedulingMode":"enabled"}});
        let hello = super::super::local_hello(
            &live,
            &live.info.read().unwrap(),
            &live.active,
            vec![],
            &config,
            false,
        );
        assert!(hello.get("settings").is_none());
        let requested = json!({"settingsPending":true,"settings":{"maxConcurrency":1}});
        let hello = super::super::local_hello(
            &live,
            &live.info.read().unwrap(),
            &live.active,
            vec![],
            &requested,
            false,
        );
        assert_eq!(hello["settings"]["maxConcurrency"], 1);
        Ok(())
    }
}
