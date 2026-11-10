use crate::{
    bridge,
    settings::{self, Settings, Target},
    wsl::{self, Distribution},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub platform: String,
    pub workers: Vec<Value>,
    pub wsl_distributions: Vec<Distribution>,
    pub settings: PublicSettings,
    pub native_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSettings {
    pub start_at_sign_in: bool,
    pub native_cli_path: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub worker_id: String,
    pub action: String,
    pub job_id: Option<String>,
    pub confirmed: Option<bool>,
    pub budget: Option<gridstator_local_control_protocol::Budget>,
    pub start_at_sign_in: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Logs {
    pub worker_id: String,
    pub job_id: String,
    pub after_seq: Option<u64>,
    pub limit: Option<u32>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Setup {
    pub mode: String,
    pub distribution: Option<String>,
    pub linux_user: Option<String>,
    pub connection: Option<String>,
    pub controller_url: Option<String>,
    pub ca_fingerprint: Option<String>,
    pub token: Option<String>,
    pub name: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Install {
    pub distribution: Option<String>,
    pub linux_user: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSettings {
    pub start_at_sign_in: Option<bool>,
    pub native_cli_path: Option<String>,
}

pub struct Backend {
    pub settings_path: PathBuf,
    pub resources: Option<PathBuf>,
    operation: Mutex<()>,
}
impl Backend {
    pub fn new(resources: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            settings_path: settings::path()?,
            resources,
            operation: Mutex::new(()),
        })
    }
    fn native_path(&self, settings: &Settings) -> Result<PathBuf> {
        if settings.native_cli_path.is_none()
            && let Some(resources) = &self.resources
        {
            let bundled = resources.join("resources/bin/gridstator");
            if bundled.is_file() {
                return Ok(bundled);
            }
            let bundled = resources.join("bin/gridstator");
            if bundled.is_file() {
                return Ok(bundled);
            }
        }
        bridge::local_cli(settings.native_cli_path.as_deref())
    }
    async fn local(
        &self,
        settings: &Settings,
        request: &Value,
        timeout: Duration,
    ) -> Result<Value> {
        let path = self.native_path(settings)?;
        bridge::verify(&path).await?;
        bridge::execute(&path, request, timeout).await
    }
    /// Called only by the OS login helper, never by refresh or a normal GUI launch.
    pub async fn sign_in_startup(&self, session: &str) -> Result<()> {
        use fs2::FileExt;
        let _guard = self.operation.lock().await;
        let mut settings = settings::load(&self.settings_path)?;
        if settings.last_startup_session.as_deref() == Some(session) {
            return Ok(());
        }
        // Serialize independent login helpers before the GUI singleton is established.
        if !self.settings_path.exists() {
            settings::save(&self.settings_path, &settings)?;
        }
        let path = self.settings_path.with_file_name("login-startup.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(path)?;
        if lock.try_lock_exclusive().is_err() {
            return Ok(());
        }
        settings = settings::load(&self.settings_path)?;
        if settings.last_startup_session.as_deref() == Some(session) {
            return Ok(());
        }
        let response = self
            .local(
                &settings,
                &bridge::worker_request("list"),
                Duration::from_secs(15),
            )
            .await?;
        settings.last_startup_session = Some(session.into());
        settings::save(&self.settings_path, &settings)?;
        let mut failures = vec![];
        for worker in response["workers"]
            .as_array()
            .context("Native worker list is invalid")?
        {
            if worker["startAtSignIn"] != true
                || worker["running"] == true
                || worker["draining"] == true
            {
                continue;
            }
            let mut request = bridge::worker_request("start");
            identify_value(&mut request, worker)?;
            if let Err(error) = self
                .local(&settings, &request, Duration::from_secs(120))
                .await
            {
                failures.push(bridge::redact(&error.to_string()));
            }
        }
        ensure!(
            failures.is_empty(),
            "Some workers could not start at sign-in: {}",
            failures.join("; ")
        );
        Ok(())
    }
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let _guard = self.operation.lock().await;
        let mut settings = settings::load(&self.settings_path)?;
        self.snapshot_inner(&mut settings).await
    }
    async fn snapshot_inner(&self, settings: &mut Settings) -> Result<Snapshot> {
        let mut snapshot = Snapshot {
            platform: platform().into(),
            workers: vec![],
            wsl_distributions: vec![],
            settings: PublicSettings {
                start_at_sign_in: settings.start_at_sign_in,
                native_cli_path: settings.native_cli_path.clone(),
            },
            native_available: false,
            error: None,
        };
        if cfg!(windows) {
            let distributions = match wsl::distributions().await {
                Ok(entries) => entries,
                Err(error) => {
                    snapshot.error = Some(format!(
                        "WSL is unavailable. Install WSL2 and a Linux distribution, then retry. {}",
                        bridge::redact(&error.to_string())
                    ));
                    vec![]
                }
            };
            for target in &mut settings.targets {
                let distribution = distributions.iter().find(|distribution| {
                    distribution.registration == target.registration
                        && distribution.name == target.distribution
                });
                if let Some(distribution) = distribution.filter(|distribution| {
                    distribution.version == 2 && distribution.state == "running"
                }) {
                    let _ = distribution;
                    let mut request = bridge::worker_request("status");
                    identify(&mut request, target);
                    match wsl::execute(target, &request, Duration::from_secs(15)).await {
                        Ok(value) => {
                            snapshot.native_available = true;
                            if let Some(worker) = value["workers"]
                                .as_array()
                                .and_then(|workers| workers.first())
                            {
                                target.cached = worker.clone();
                            }
                        }
                        Err(error) => {
                            target.cached["error"] = json!(bridge::redact(&error.to_string()));
                            target.cached["running"] = json!(false);
                            target.cached["connected"] = json!(false);
                            target.cached["state"] = json!("unavailable");
                        }
                    }
                } else {
                    target.cached["running"] = json!(false);
                    target.cached["connected"] = json!(false);
                    target.cached["state"] = json!(if distribution.is_some() {
                        "stopped"
                    } else {
                        "unavailable"
                    });
                    target.cached["activeJobs"] = json!([]);
                }
                snapshot
                    .workers
                    .push(present(&target.cached, Some(target))?);
            }
            snapshot.wsl_distributions = distributions;
            settings::save(&self.settings_path, settings)?;
        } else {
            match self
                .local(
                    settings,
                    &bridge::worker_request("list"),
                    Duration::from_secs(15),
                )
                .await
            {
                Ok(value) => {
                    snapshot.native_available = true;
                    for worker in value["workers"]
                        .as_array()
                        .context("Native worker list is invalid")?
                    {
                        snapshot.workers.push(present(worker, None)?);
                    }
                }
                Err(error) => snapshot.error = Some(bridge::redact(&error.to_string())),
            }
        }
        Ok(snapshot)
    }
    async fn target(
        &self,
        settings: &Settings,
        worker_id: &str,
    ) -> Result<(Value, Option<Target>)> {
        if cfg!(windows) {
            let target = settings
                .targets
                .iter()
                .find(|target| worker_id_for(&target.cached, Some(target)) == worker_id)
                .context("This worker is no longer configured; refresh and select it again")?
                .clone();
            ensure!(
                wsl::distributions()
                    .await?
                    .iter()
                    .any(
                        |distribution| distribution.registration == target.registration
                            && distribution.name == target.distribution
                            && distribution.version == 2
                    ),
                "The configured WSL2 distribution has changed or is unavailable; select it again"
            );
            return Ok((target.cached.clone(), Some(target)));
        }
        let response = self
            .local(
                settings,
                &bridge::worker_request("list"),
                Duration::from_secs(15),
            )
            .await?;
        let worker = response["workers"]
            .as_array()
            .context("Native worker list is invalid")?
            .iter()
            .find(|worker| worker_id_for(worker, None) == worker_id)
            .context("This worker identity changed; refresh and select it again")?
            .clone();
        Ok((worker, None))
    }
    async fn execute_target(
        &self,
        settings: &Settings,
        target: Option<&Target>,
        request: &Value,
    ) -> Result<Value> {
        if let Some(target) = target {
            wsl::execute(target, request, Duration::from_secs(120)).await
        } else {
            self.local(settings, request, Duration::from_secs(120))
                .await
        }
    }
    pub async fn action(&self, action: Action) -> Result<Snapshot> {
        let _guard = self.operation.lock().await;
        ensure!(
            [
                "pause",
                "resume",
                "setBudget",
                "start",
                "stop",
                "forceStop",
                "cancel",
                "setStartup"
            ]
            .contains(&action.action.as_str()),
            "Unsupported worker action"
        );
        if action.action == "forceStop" {
            ensure!(
                action.confirmed == Some(true),
                "Confirm force-stop before cancelling running jobs"
            );
        }
        if let Some(budget) = &action.budget {
            budget.validate().map_err(anyhow::Error::msg)?;
        }
        let mut settings = settings::load(&self.settings_path)?;
        let (worker, target) = self.target(&settings, &action.worker_id).await?;
        let mut request = bridge::worker_request(&action.action);
        identify_value(&mut request, &worker)?;
        for (key, value) in [
            ("jobId", json!(action.job_id)),
            ("confirmed", json!(action.confirmed)),
            ("budget", json!(action.budget)),
            ("startAtSignIn", json!(action.start_at_sign_in)),
        ] {
            if !value.is_null() {
                request[key] = value;
            }
        }
        self.execute_target(&settings, target.as_ref(), &request)
            .await?;
        if let Some(start) = action.start_at_sign_in {
            if let Some(entry) = settings
                .targets
                .iter_mut()
                .find(|target| worker_id_for(&target.cached, Some(target)) == action.worker_id)
            {
                entry.start_at_sign_in = start;
            }
            settings::save(&self.settings_path, &settings)?;
        }
        self.snapshot_inner(&mut settings).await
    }
    pub async fn logs(&self, logs: Logs) -> Result<Value> {
        let _guard = self.operation.lock().await;
        ensure!(
            logs.job_id.len() <= 256 && logs.limit.unwrap_or(500) <= 2000,
            "Invalid log request"
        );
        let settings = settings::load(&self.settings_path)?;
        let (worker, target) = self.target(&settings, &logs.worker_id).await?;
        let mut request = bridge::worker_request("logs");
        identify_value(&mut request, &worker)?;
        request["jobId"] = json!(logs.job_id);
        request["afterSeq"] = json!(logs.after_seq.unwrap_or(0));
        request["limit"] = json!(logs.limit.unwrap_or(500));
        let response = self
            .execute_target(&settings, target.as_ref(), &request)
            .await?;
        Ok(response
            .get("logs")
            .context("No logs were returned for this job")?
            .clone())
    }
    pub async fn change_settings(&self, request: ChangeSettings) -> Result<Snapshot> {
        let _guard = self.operation.lock().await;
        let mut settings = settings::load(&self.settings_path)?;
        if let Some(path) = request.native_cli_path {
            ensure!(
                !cfg!(windows),
                "Choose the native helper inside the selected WSL distribution"
            );
            if path.trim().is_empty() {
                settings.native_cli_path = None;
            } else {
                let resolved = bridge::local_cli(Some(path.trim()))?;
                bridge::verify(&resolved).await?;
                settings.native_cli_path = Some(resolved.to_string_lossy().into_owned());
            }
        }
        if let Some(start) = request.start_at_sign_in {
            settings.start_at_sign_in = start;
        }
        settings::save(&self.settings_path, &settings)?;
        self.snapshot_inner(&mut settings).await
    }
    async fn wsl_target(&self, request: &Install) -> Result<Target> {
        let name = request
            .distribution
            .as_deref()
            .context("Choose an existing WSL2 distribution before continuing")?;
        let distribution = wsl::distributions()
            .await?
            .into_iter()
            .find(|distribution| distribution.name == name)
            .context("The selected WSL distribution is unavailable")?;
        ensure!(
            distribution.version == 2,
            "Select a WSL2 distribution; this companion does not convert WSL1 distributions"
        );
        let (linux_user, home) = wsl::resolve_account(name, request.linux_user.as_deref()).await?;
        Ok(Target {
            distribution: name.into(),
            registration: distribution.registration,
            linux_user,
            cli_path: format!("{home}/.local/bin/gridstator"),
            data_dir: String::new(),
            node_id: String::new(),
            cluster_id: None,
            ca_fingerprint: String::new(),
            start_at_sign_in: true,
            cached: json!({}),
        })
    }
    async fn installer_arguments(
        &self,
        target: &str,
        prefix: &str,
        wsl_target: Option<&Target>,
    ) -> Result<Vec<String>> {
        let mut arguments = vec!["-s".into(), "--".into()];
        if let Some(bundle) = bridge::bundled_release(self.resources.as_deref(), target)? {
            let mut directory = bundle.directory.to_string_lossy().into_owned();
            if let Some(wsl_target) = wsl_target {
                // wslpath resolves the selected distribution's mount configuration. Keep paths
                // in argv: Windows account and application paths may contain spaces.
                let windows_path = directory.strip_prefix(r"\\?\").unwrap_or(&directory);
                let bytes = bridge::output(
                    wsl::command(
                        &wsl_target.distribution,
                        Some(&wsl_target.linux_user),
                        "/usr/bin/wslpath",
                        &["-a", "-u", windows_path],
                    )?,
                    &[],
                    Duration::from_secs(20),
                )
                .await?;
                directory = String::from_utf8(bytes)?.trim().to_owned();
                ensure!(
                    directory.starts_with('/') && !directory.chars().any(char::is_control),
                    "WSL could not resolve the bundled native CLI path"
                );
            }
            arguments.extend([
                "--tag".into(),
                bundle.tag,
                "--bundle-dir".into(),
                directory,
                "--manifest-sha256".into(),
                bundle.manifest_sha256,
                "--archive-sha256".into(),
                bundle.archive_sha256,
            ]);
        } else {
            let tag = bridge::install_release_tag(&target.replace('/', "-")).await?;
            arguments.extend(["--tag".into(), tag]);
        }
        arguments.extend(["--prefix".into(), prefix.into(), "--if-missing".into()]);
        Ok(arguments)
    }
    async fn install_inner(
        &self,
        settings: &mut Settings,
        request: &Install,
    ) -> Result<Option<Target>> {
        let script = include_bytes!("../resources/install-native.sh");
        if cfg!(windows) {
            let target = self.wsl_target(request).await?;
            let output = bridge::output(
                wsl::command(
                    &target.distribution,
                    Some(&target.linux_user),
                    "/usr/bin/uname",
                    &["-m"],
                )?,
                &[],
                Duration::from_secs(20),
            )
            .await?;
            let arch = match String::from_utf8(output)?.trim() {
                "x86_64" => "amd64",
                "aarch64" | "arm64" => "arm64",
                _ => bail!("This WSL architecture is unavailable for native installation"),
            };
            let prefix = target
                .cli_path
                .strip_suffix("/bin/gridstator")
                .context("Invalid WSL CLI path")?;
            let arguments = self
                .installer_arguments(&format!("linux/{arch}"), prefix, Some(&target))
                .await?;
            let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
            bridge::output(
                wsl::command(
                    &target.distribution,
                    Some(&target.linux_user),
                    "/bin/sh",
                    &arguments,
                )?,
                script,
                Duration::from_secs(300),
            )
            .await?;
            wsl::verify(&target).await?;
            self.remember_wsl(settings, &target).await?;
            Ok(Some(target))
        } else {
            ensure!(
                settings.native_cli_path.is_none(),
                "A native CLI path is configured. Update that trusted installation or clear the path before installing another release"
            );
            let os = if cfg!(target_os = "macos") {
                "darwin"
            } else {
                "linux"
            };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                _ => bail!("This architecture is unavailable for native installation"),
            };
            let prefix = dirs::home_dir()
                .context("This account has no home directory")?
                .join(".local");
            let arguments = self
                .installer_arguments(&format!("{os}/{arch}"), &prefix.to_string_lossy(), None)
                .await?;
            let mut command = bridge::command(std::path::Path::new("/bin/sh"));
            command.args(arguments);
            bridge::output(command, script, Duration::from_secs(300)).await?;
            let cli = prefix.join("bin/gridstator");
            bridge::verify(&cli).await?;
            settings.native_cli_path = Some(cli.to_string_lossy().into_owned());
            settings::save(&self.settings_path, settings)?;
            Ok(None)
        }
    }
    pub async fn install(&self, request: Install) -> Result<Snapshot> {
        let _guard = self.operation.lock().await;
        let mut settings = settings::load(&self.settings_path)?;
        self.install_inner(&mut settings, &request).await?;
        self.snapshot_inner(&mut settings).await
    }
    async fn remember_wsl(&self, settings: &mut Settings, target: &Target) -> Result<()> {
        let response = wsl::execute(
            target,
            &bridge::worker_request("list"),
            Duration::from_secs(30),
        )
        .await?;
        for worker in response["workers"]
            .as_array()
            .context("WSL worker list is invalid")?
        {
            let mut saved = target.clone();
            saved.data_dir = worker["dataDir"]
                .as_str()
                .context("Worker data directory unavailable")?
                .into();
            saved.node_id = worker["nodeId"]
                .as_str()
                .context("Worker identity unavailable")?
                .into();
            saved.cluster_id = worker["clusterId"].as_str().map(str::to_owned);
            saved.ca_fingerprint = worker["caFingerprint"]
                .as_str()
                .context("Worker CA identity unavailable")?
                .into();
            saved.start_at_sign_in = worker["startAtSignIn"].as_bool().unwrap_or(true);
            saved.cached = worker.clone();
            if let Some(existing) = settings.targets.iter_mut().find(|existing| {
                existing.registration == saved.registration
                    && existing.linux_user == saved.linux_user
                    && existing.data_dir == saved.data_dir
            }) {
                *existing = saved;
            } else {
                settings.targets.push(saved);
            }
        }
        settings::save(&self.settings_path, settings)
    }
    pub async fn setup(&self, setup: Setup) -> Result<Snapshot> {
        let _guard = self.operation.lock().await;
        ensure!(
            ["join", "create"].contains(&setup.mode.as_str()),
            "Choose Join a controller or Create a controller here"
        );
        let credentials = if setup.mode == "join" {
            Some(bridge::join_credentials(
                setup.connection.as_deref(),
                setup.controller_url.as_deref(),
                setup.ca_fingerprint.as_deref(),
                setup.token.as_deref(),
            )?)
        } else {
            None
        };
        let mut settings = settings::load(&self.settings_path)?;
        let install = Install {
            distribution: setup.distribution,
            linux_user: setup.linux_user,
        };
        let target = if cfg!(windows) {
            let target = self.wsl_target(&install).await?;
            if wsl::verify(&target).await.is_err() {
                self.install_inner(&mut settings, &install).await?
            } else {
                Some(target)
            }
        } else {
            if self.native_path(&settings).is_err() {
                self.install_inner(&mut settings, &install).await?;
            }
            None
        };
        let mut request = if let Some(credentials) = credentials {
            json!({"version":1,"operation":"worker","action":"join","url":credentials.url,"fingerprint":credentials.fingerprint,"token":credentials.token,"name":setup.name})
        } else {
            let paths = self
                .execute_target(
                    &settings,
                    target.as_ref(),
                    &json!({"version":1,"operation":"paths"}),
                )
                .await?;
            json!({"version":1,"operation":"setup","configPath":paths["configPath"]})
        };
        if setup.mode == "join" {
            request["startAtSignIn"] = json!(true);
        }
        self.execute_target(&settings, target.as_ref(), &request)
            .await?;
        if let Some(target) = target {
            self.remember_wsl(&mut settings, &target).await?;
        }
        self.snapshot_inner(&mut settings).await
    }
}
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}
fn identify(request: &mut Value, target: &Target) {
    request["dataDir"] = json!(target.data_dir);
    request["nodeId"] = json!(target.node_id);
    request["caFingerprint"] = json!(target.ca_fingerprint);
    if let Some(cluster) = &target.cluster_id {
        request["clusterId"] = json!(cluster);
    }
}
fn identify_value(request: &mut Value, worker: &Value) -> Result<()> {
    for key in ["dataDir", "nodeId", "caFingerprint"] {
        request[key] = json!(
            worker[key]
                .as_str()
                .context("The worker’s saved identity is incomplete")?
        );
    }
    if let Some(cluster) = worker["clusterId"].as_str() {
        request["clusterId"] = json!(cluster);
    }
    Ok(())
}
fn worker_id_for(worker: &Value, target: Option<&Target>) -> String {
    let parts = json!([
        target.map(|target| target.registration.as_str()),
        target.map(|target| target.linux_user.as_str()),
        worker["dataDir"],
        worker["nodeId"],
        worker["clusterId"],
        worker["caFingerprint"]
    ]);
    format!("{:x}", Sha256::digest(parts.to_string().as_bytes()))
}
fn present(worker: &Value, target: Option<&Target>) -> Result<Value> {
    ensure!(worker.is_object(), "Worker status is invalid");
    let mut result = worker.clone();
    result["id"] = json!(worker_id_for(worker, target));
    result["label"] = json!(worker["name"].as_str().unwrap_or("Local worker"));
    if let Some(target) = target {
        result["distribution"] = json!(target.distribution);
        result["linuxUser"] = json!(target.linux_user);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_labels_and_identity_includes_cluster() {
        let worker = json!({"dataDir":"/private/agent","nodeId":"node1","clusterId":"cluster1","name":"Old"});
        let mut renamed = worker.clone();
        renamed["name"] = json!("New");
        assert_eq!(worker_id_for(&worker, None), worker_id_for(&renamed, None));
        renamed["clusterId"] = json!("cluster2");
        assert_ne!(worker_id_for(&worker, None), worker_id_for(&renamed, None));
    }
    #[tokio::test]
    async fn rejects_arbitrary_actions_and_unconfirmed_force_stop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let backend = Backend {
            settings_path: directory.path().join("settings.json"),
            resources: None,
            operation: Mutex::new(()),
        };
        for (action, confirmed) in [("shell", None), ("forceStop", Some(false))] {
            assert!(
                backend
                    .action(Action {
                        worker_id: "none".into(),
                        action: action.into(),
                        job_id: None,
                        confirmed,
                        budget: None,
                        start_at_sign_in: None
                    })
                    .await
                    .is_err()
            );
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    const FAKE_HELPER: &str = r#"#!/usr/bin/python3
import sys,json,pathlib
root=pathlib.Path(__file__).parent
if sys.argv[1:]==['build-info','--json']:
 print(json.dumps({'version':'2.0.0-dev.1','capabilities':['bridge-v1','worker-control-v1','local-setup-v1']}));sys.exit()
assert sys.argv[1:]==['bridge'], 'requests must not appear in argv'
r=json.load(sys.stdin)
with open(root/'requests.jsonl','a') as f:f.write(json.dumps(r)+'\n')
batch=root/'workers.json'
if batch.exists():
 rows=json.load(open(batch))
 if r.get('action')=='start':
  w=next(w for w in rows if w['nodeId']==r['nodeId'])
  assert w['caFingerprint']==r['caFingerprint']
  w['running']=True
  json.dump(rows,open(batch,'w'))
 print(json.dumps({'version':1,'workers':rows}));sys.exit()
p=root/'worker.json'
w=json.load(open(p)) if p.exists() else {'dataDir':'/private/agent','nodeId':'node1','clusterId':None,'caFingerprint':'ab'*32,'name':'Owned worker','controllerUrl':'https://controller:7300','embedded':False,'running':True,'connected':False,'state':'disconnected','paused':False,'draining':False,'startAtSignIn':True,'budget':{},'generation':1,'activeJobs':[],'recentJobs':[]}
a=r.get('action')
if a in ['pause','resume']:
 assert r['nodeId']==w['nodeId'] and r['caFingerprint']==w['caFingerprint']
 w['paused']=a=='pause';w['generation']+=1
elif a=='setBudget':w['budget']=r['budget']
elif a=='setStartup':w['startAtSignIn']=r['startAtSignIn']
elif a=='join':assert r['token'].startswith('gsj_')
json.dump(w,open(p,'w'))
print(json.dumps({'version':1,'workers':[w]}))
"#;
    #[tokio::test]
    async fn owned_worker_controls_use_private_stdin_and_revalidate_identity() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let helper = directory.path().join("gridstator");
        std::fs::write(&helper, FAKE_HELPER)?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        let settings_path = directory.path().join("desktop/settings.json");
        settings::save(
            &settings_path,
            &Settings {
                native_cli_path: Some(helper.to_string_lossy().into()),
                ..Settings::default()
            },
        )?;
        let backend = Backend {
            settings_path,
            resources: None,
            operation: Mutex::new(()),
        };
        let snapshot = backend.snapshot().await?;
        assert!(snapshot.native_available, "{:?}", snapshot.error);
        let id = snapshot.workers[0]["id"]
            .as_str()
            .context("worker ID")?
            .to_owned();
        let request = Action {
            worker_id: id.clone(),
            action: "pause".into(),
            job_id: None,
            confirmed: None,
            budget: None,
            start_at_sign_in: None,
        };
        assert_eq!(
            backend.action(request.clone()).await?.workers[0]["paused"],
            true
        );
        let pasted = format!(
            "gridstator agent join https://controller:7300 --fingerprint {} --token gsj_private",
            "ab".repeat(32)
        );
        backend
            .setup(Setup {
                mode: "join".into(),
                connection: Some(pasted),
                ..Setup::default()
            })
            .await?;
        let requests = std::fs::read_to_string(directory.path().join("requests.jsonl"))?;
        assert!(
            requests
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .any(|request| request["action"] == "join" && request["token"] == "gsj_private")
        );
        let path = directory.path().join("worker.json");
        let mut worker: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        worker["caFingerprint"] = json!("cd".repeat(32));
        std::fs::write(&path, serde_json::to_vec(&worker)?)?;
        assert!(
            backend.action(request).await.is_err(),
            "A saved UI target must not control a replacement identity"
        );
        Ok(())
    }
    #[tokio::test]
    async fn login_startup_is_independent_of_gui_preference_and_preserves_pause() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let helper = directory.path().join("gridstator");
        std::fs::write(&helper, FAKE_HELPER)?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        let settings_path = directory.path().join("desktop/settings.json");
        settings::save(
            &settings_path,
            &Settings {
                start_at_sign_in: false,
                native_cli_path: Some(helper.to_string_lossy().into()),
                ..Settings::default()
            },
        )?;
        let backend = Backend {
            settings_path,
            resources: None,
            operation: Mutex::new(()),
        };
        let base = json!({"dataDir":"/private/agent","nodeId":"paused","clusterId":null,"caFingerprint":"ab".repeat(32),"name":"Paused","running":false,"paused":true,"draining":false,"startAtSignIn":true});
        let mut opted_out = base.clone();
        opted_out["nodeId"] = json!("disabled");
        opted_out["startAtSignIn"] = json!(false);
        let mut running = base.clone();
        running["nodeId"] = json!("already-running");
        running["running"] = json!(true);
        let path = directory.path().join("workers.json");
        std::fs::write(&path, serde_json::to_vec(&vec![base, opted_out, running])?)?;
        backend.sign_in_startup("login-one").await?;
        let mut rows: Vec<Value> = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(rows[0]["running"], true);
        assert_eq!(rows[0]["paused"], true);
        assert_eq!(rows[1]["running"], false);
        rows[0]["running"] = json!(false);
        std::fs::write(&path, serde_json::to_vec(&rows)?)?;
        backend.sign_in_startup("login-one").await?;
        rows = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(
            rows[0]["running"], false,
            "Reopening or duplicate events cannot undo a stop in this login"
        );
        backend.sign_in_startup("login-two").await?;
        rows = serde_json::from_slice(&std::fs::read(&path)?)?;
        assert_eq!(rows[0]["running"], true);
        assert_eq!(rows[0]["paused"], true);
        let requests: Vec<Value> =
            std::fs::read_to_string(directory.path().join("requests.jsonl"))?
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        let starts: Vec<_> = requests
            .iter()
            .filter(|request| request["action"] == "start")
            .collect();
        assert_eq!(starts.len(), 2);
        assert!(starts.iter().all(|request| request["nodeId"] == "paused"));
        assert!(requests.iter().all(|request| request["action"] != "resume"));
        Ok(())
    }
}
