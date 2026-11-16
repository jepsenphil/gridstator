//! Resumable Windows prerequisites. Plans contain no controller credentials and never enroll or
//! replace a worker. The background Windows host owns execution; the webview only polls progress.
use crate::{
    bridge,
    settings::{self, Target},
    wsl::Distribution,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const DEDICATED: &str = "Gridstator-Ubuntu";
const ACCOUNT: &str = "gridstator";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Engine,
    Desktop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub action: String,
    pub revision: Option<String>,
    pub engine: Option<Engine>,
    pub distribution: Option<String>,
    pub confirmed: Option<bool>,
    pub accept_docker_terms: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    pub label: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub supported: bool,
    pub revision: String,
    pub phase: String,
    pub engine: Option<Engine>,
    pub distribution: String,
    pub linux_user: Option<String>,
    pub wsl_available: bool,
    pub existing_worker: bool,
    pub owned_distribution: bool,
    pub message: String,
    pub error: Option<String>,
    pub manual_action: Option<String>,
    pub docker_terms_required: bool,
    pub steps: Vec<Step>,
    pub distributions: Vec<Distribution>,
    pub can_retry: bool,
    pub can_restart: bool,
    pub can_delete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    schema_version: u32,
    pub status: Status,
    initial_wsl_available: bool,
    approved: bool,
    accepted_docker_terms: bool,
    ownership_id: String,
    owned_registration: Option<String>,
    creation_requested: bool,
    #[serde(default)]
    restart_boot_id: Option<String>,
    #[serde(default)]
    existing_target: Option<Target>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Report {
    pub wsl_available: bool,
    pub desktop_installed: bool,
    pub detected_engine: Option<Engine>,
    pub linux_user: Option<String>,
    pub distributions: Vec<Distribution>,
    pub compatible: bool,
    pub registration: Option<String>,
    pub owned: bool,
    pub restart_required: bool,
    pub boot_id: Option<String>,
    pub wsl_ready: bool,
    pub action_required: Option<String>,
}

type Task<'a> = Pin<Box<dyn Future<Output = Result<Report>> + Send + 'a>>;
pub(crate) trait Executor: Send + Sync {
    fn execute<'a>(&'a self, action: &'a str, record: &'a Record) -> Task<'a>;
}

struct Windows;
impl Executor for Windows {
    fn execute<'a>(&'a self, action: &'a str, record: &'a Record) -> Task<'a> {
        Box::pin(async move {
            #[cfg(windows)]
            {
                let system = PathBuf::from(
                    std::env::var_os("SystemRoot").context("Windows directory unavailable")?,
                );
                let executable = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
                let mut command = bridge::command(&executable);
                ensure!(
                    include_str!("../windows/prerequisites.ps1")
                        .encode_utf16()
                        .count()
                        < 26_000,
                    "The Windows setup helper exceeds its safe command-line bound"
                );
                command.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    include_str!("../windows/prerequisites.ps1"),
                ]);
                let input = serde_json::json!({
                    "action": action, "distribution":record.status.distribution,
                    "linuxUser":record.status.linux_user, "engine":record.status.engine,
                    "ownershipId":record.ownership_id,"ownedRegistration":record.owned_registration,
                    "creationRequested":record.creation_requested,
                    "acceptDockerTerms":record.accepted_docker_terms,
                    "executable":std::env::current_exe()?,
                });
                let output = bridge::output(
                    command,
                    &serde_json::to_vec(&input)?,
                    std::time::Duration::from_secs(1800),
                )
                .await?;
                serde_json::from_slice(&output)
                    .context("Windows setup returned an invalid response")
            }
            #[cfg(not(windows))]
            {
                let _ = (action, record);
                anyhow::bail!("Automatic Windows setup is available on Windows only")
            }
        })
    }
}

/// Start the reviewed Desktop runtime only for a worker opted into this OS sign-in.
/// This leaves Docker's own login preferences unchanged and never runs on ordinary refresh.
pub async fn start_runtime_at_sign_in(settings_path: &Path, targets: &[Target]) -> Result<()> {
    if let Some(record) = load(settings_path)? {
        start_login_runtime(&Windows, &record, targets).await?;
    }
    Ok(())
}
async fn start_login_runtime(
    executor: &dyn Executor,
    record: &Record,
    targets: &[Target],
) -> Result<()> {
    if record.approved
        && record.status.phase == "ready"
        && record.status.engine == Some(Engine::Desktop)
        && targets.iter().any(|target| {
            target.start_at_sign_in
                && target.distribution == record.status.distribution
                && Some(&target.linux_user) == record.status.linux_user.as_ref()
        })
    {
        executor.execute("openDocker", record).await?;
    }
    Ok(())
}

fn state_path(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("windows-setup.json")
}
fn lock_path(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("windows-setup.lock")
}
fn new_id() -> String {
    format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
fn load(settings_path: &Path) -> Result<Option<Record>> {
    let path = state_path(settings_path);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 * 1024,
            "Windows setup state is not a regular bounded file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let record: Record = serde_json::from_slice(&fs::read(path)?)
        .context("Windows setup state is invalid; restore it before retrying")?;
    ensure!(
        record.schema_version == 1,
        "Unsupported Windows setup state version"
    );
    validate_distribution(&record.status.distribution)?;
    ensure!(
        record
            .ownership_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-'),
        "Invalid setup ownership identity"
    );
    Ok(Some(record))
}
fn save(settings_path: &Path, record: &Record) -> Result<()> {
    settings::save_json(&state_path(settings_path), record)
}
pub fn persisted(settings_path: &Path) -> Option<Status> {
    load(settings_path).ok().flatten().map(|r| r.status)
}
pub(crate) fn prepared_target(settings_path: &Path) -> Result<Option<(String, Option<String>)>> {
    let Some(record) = load(settings_path)? else {
        return Ok(None);
    };
    ensure!(
        record.status.phase == "ready",
        "Finish Windows runtime setup before joining or creating a controller"
    );
    Ok(Some((record.status.distribution, record.status.linux_user)))
}
pub(crate) fn remember_workers(settings_path: &Path, targets: &[Target]) -> Result<()> {
    if let Some(mut record) = load(settings_path)? {
        record.existing_target = targets
            .iter()
            .find(|target| target.distribution == record.status.distribution)
            .map(|target| Target {
                cached: serde_json::json!({}),
                ..target.clone()
            });
        record.status.existing_worker = record.existing_target.is_some();
        save(settings_path, &record)?;
    }
    Ok(())
}
async fn check_managed_worker(target: &Target) -> Result<()> {
    let mut request = bridge::worker_request("status");
    request["dataDir"] = serde_json::json!(target.data_dir);
    request["nodeId"] = serde_json::json!(target.node_id);
    request["caFingerprint"] = serde_json::json!(target.ca_fingerprint);
    let response =
        crate::wsl::execute(target, &request, std::time::Duration::from_secs(30)).await?;
    let worker = response["workers"]
        .as_array()
        .and_then(|workers| {
            workers
                .iter()
                .find(|worker| worker["nodeId"] == target.node_id)
        })
        .context("The configured worker could not be identified")?;
    ensure!(
        worker["connected"] == true
            && worker["info"]["build"]["managed"] == true
            && worker["info"]["docker"]["available"] == true,
        "The existing worker must be connected through a verified managed service. Start or repair it before continuing."
    );
    Ok(())
}
fn validate_distribution(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('-')
            && !name.chars().any(|c| c.is_control()
                || matches!(
                    c,
                    '/' | '\\' | '"' | '\'' | '`' | '$' | ';' | '|' | '&' | '<' | '>'
                )),
        "Choose a valid WSL distribution"
    );
    Ok(())
}
fn initial() -> Record {
    let revision = new_id();
    Record {
        schema_version: 1,
        initial_wsl_available: false,
        approved: false,
        accepted_docker_terms: false,
        ownership_id: revision.clone(),
        owned_registration: None,
        creation_requested: false,
        restart_boot_id: None,
        existing_target: None,
        status: Status {
            supported: cfg!(windows),
            revision,
            phase: "review".into(),
            engine: None,
            distribution: DEDICATED.into(),
            linux_user: Some(ACCOUNT.into()),
            wsl_available: false,
            existing_worker: false,
            owned_distribution: false,
            message: "Review this machine’s setup before installing prerequisites.".into(),
            error: None,
            manual_action: None,
            docker_terms_required: false,
            steps: [
                ("wsl", "Prepare WSL2"),
                ("distribution", "Prepare Ubuntu"),
                ("runtime", "Prepare Docker"),
                ("verify", "Verify container access"),
            ]
            .into_iter()
            .map(|(id, label)| Step {
                id: id.into(),
                label: label.into(),
                state: "pending".into(),
            })
            .collect(),
            distributions: vec![],
            can_retry: false,
            can_restart: false,
            can_delete: false,
        },
    }
}

fn selected_target<'a>(
    record: &Record,
    request: &Request,
    targets: &'a [Target],
    fresh: bool,
) -> Option<&'a Target> {
    if fresh && request.distribution.is_none() {
        targets.first()
    } else {
        let selected = request
            .distribution
            .as_deref()
            .unwrap_or(&record.status.distribution);
        targets
            .iter()
            .find(|target| target.distribution == selected)
    }
}

fn plan(
    record: &mut Record,
    report: Report,
    request: &Request,
    targets: &[Target],
    fresh: bool,
) -> Result<()> {
    if fresh {
        record.initial_wsl_available = report.wsl_available;
    }
    record.status.wsl_available = report.wsl_available;
    record.status.distributions = report.distributions;
    record.status.existing_worker = false;
    record.existing_target = None;
    if let Some(target) = selected_target(record, request, targets, fresh) {
        record.status.distribution = target.distribution.clone();
        record.status.linux_user = Some(target.linux_user.clone());
        ensure!(
            request.engine.is_none_or(|selected| report
                .detected_engine
                .is_none_or(|actual| selected == actual)),
            "This configured worker already uses another engine. Preserve it, or migrate its engine explicitly after stopping work; setup will not silently switch its runtime."
        );
        record.status.existing_worker = true;
        record.existing_target = Some(Target {
            cached: serde_json::json!({}),
            ..target.clone()
        });
    } else if let Some(name) = &request.distribution {
        validate_distribution(name)?;
        ensure!(
            name == DEDICATED
                || record
                    .status
                    .distributions
                    .iter()
                    .any(|d| d.name == *name && d.version == 2),
            "Choose an available WSL2 distribution"
        );
        record.status.distribution = name.clone();
        record.status.linux_user = if name == DEDICATED {
            Some(ACCOUNT.into())
        } else {
            report.linux_user
        };
        record.status.existing_worker = false;
    }
    record.status.engine = request
        .engine
        .or(report.detected_engine)
        .or(record.status.engine)
        .or(Some(if record.initial_wsl_available {
            Engine::Engine
        } else {
            Engine::Desktop
        }));
    record.status.docker_terms_required = record.status.engine == Some(Engine::Desktop)
        && !report.desktop_installed
        && !record.accepted_docker_terms;
    record.status.owned_distribution =
        record.status.distribution == DEDICATED && record.owned_registration.is_some();
    record.status.can_delete = record.status.owned_distribution;
    Ok(())
}

fn authorized(record: &Record, request: &Request) -> Result<()> {
    ensure!(
        request.confirmed == Some(true),
        "Review and confirm the setup changes before continuing"
    );
    ensure!(
        request.revision.as_deref() == Some(record.status.revision.as_str()),
        "The setup plan changed; review the current plan before continuing"
    );
    ensure!(
        request
            .engine
            .is_none_or(|engine| Some(engine) == record.status.engine)
            && request
                .distribution
                .as_ref()
                .is_none_or(|name| name == &record.status.distribution),
        "The selected runtime changed; inspect and review it before continuing"
    );
    Ok(())
}
fn stage(record: &mut Record, id: &str, state: &str, message: &str) {
    if let Some(step) = record.status.steps.iter_mut().find(|step| step.id == id) {
        step.state = state.into();
    }
    record.status.message = message.into();
}
fn finish_failure(record: &mut Record, error: &anyhow::Error) {
    record.status.phase = "failed".into();
    record.status.error = Some(bridge::redact(&error.to_string()));
    record.status.message="Setup stopped. Your progress has been retained; retry or explicitly choose another engine.".into();
    record.status.can_retry = true;
    record.status.can_restart = false;
    for step in &mut record.status.steps {
        if step.state == "running" {
            step.state = "blocked".into();
        }
    }
}

async fn prepare(executor: &dyn Executor, settings_path: &Path, record: &mut Record) -> Result<()> {
    let stages = if record.status.existing_worker {
        vec![(
            "verify",
            "verifyExisting",
            "Verify the existing managed worker and Docker access",
        )]
    } else {
        vec![
            ("wsl", "wsl", "Prepare WSL2"),
            (
                "distribution",
                "distribution",
                "Prepare the selected Ubuntu distribution",
            ),
            ("runtime", "runtime", "Prepare the selected Docker engine"),
            (
                "verify",
                "verify",
                "Run a container to verify Docker access",
            ),
        ]
    };
    for (id, action, message) in stages {
        stage(record, id, "running", message);
        if action == "distribution" && record.status.distribution == DEDICATED {
            record.creation_requested = true;
        }
        save(settings_path, record)?;
        let report = executor.execute(action, record).await?;
        if action == "verifyExisting"
            && let Some(target) = record.existing_target.as_ref()
        {
            check_managed_worker(target).await?;
        }
        record.status.wsl_available |= report.wsl_available;
        if let Some(user) = report.linux_user {
            record.status.linux_user = Some(user);
        }
        if report.owned {
            record.status.owned_distribution = true;
            record.owned_registration = report.registration;
            record.status.can_delete = true;
        }
        if report.restart_required {
            record.restart_boot_id = report.boot_id;
            record.status.phase = "restartRequired".into();
            record.status.can_restart = true;
            record.status.message="Windows needs a restart. Restart when convenient; Gridstator will resume this selected setup after sign-in.".into();
            save(settings_path, record)?;
            executor.execute("registerResume", record).await?;
            return Ok(());
        }
        if let Some(action) = report.action_required {
            record.status.phase = "actionRequired".into();
            record.status.manual_action = Some(action);
            record.status.message =
                "Complete the indicated step, then choose Verify and continue.".into();
            record.status.can_retry = true;
            save(settings_path, record)?;
            return Ok(());
        }
        stage(record, id, "done", message);
        save(settings_path, record)?;
    }
    record.status.phase = "ready".into();
    record.status.error = None;
    record.status.manual_action = None;
    record.status.can_retry = false;
    record.status.can_restart = false;
    record.status.can_delete = record.status.owned_distribution;
    record.status.message = if record.status.existing_worker {
        "The existing managed worker and its container runtime are ready."
    } else {
        "The container runtime is ready. Join or create a controller to finish worker setup."
    }
    .into();
    save(settings_path, record)
}

fn acquire(settings_path: &Path) -> Result<Option<fs::File>> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let path = lock_path(settings_path);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Invalid Windows setup lock"
        );
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path)?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock)),
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn execute(
    settings_path: PathBuf,
    request: Request,
    targets: Vec<Target>,
) -> Result<Status> {
    execute_with(
        settings_path,
        request,
        targets,
        Arc::new(Windows),
        cfg!(windows),
    )
    .await
}
async fn execute_with(
    settings_path: PathBuf,
    request: Request,
    targets: Vec<Target>,
    executor: Arc<dyn Executor>,
    supported: bool,
) -> Result<Status> {
    ensure!(
        [
            "inspect",
            "start",
            "retry",
            "restart",
            "verify",
            "delete",
            "openDocker",
            "openDockerTerms"
        ]
        .contains(&request.action.as_str()),
        "Unsupported Windows setup action"
    );
    let acquired = if supported {
        acquire(&settings_path)?
    } else {
        None
    };
    let loaded = load(&settings_path)?;
    let fresh = loaded.is_none();
    let mut record = loaded.unwrap_or_else(initial);
    record.status.supported = supported;
    if !supported {
        return Ok(record.status);
    }
    let Some(lock) = acquired else {
        ensure!(
            request.action == "inspect",
            "Setup is already running; refresh its progress before retrying"
        );
        return Ok(record.status);
    };
    if request.action == "inspect" {
        if record.status.phase == "running" {
            finish_failure(
                &mut record,
                &anyhow::anyhow!(
                    "Setup was interrupted. Retry will verify and reuse completed work."
                ),
            );
        }
        let changed = request
            .engine
            .is_some_and(|engine| Some(engine) != record.status.engine)
            || request
                .distribution
                .as_ref()
                .is_some_and(|name| name != &record.status.distribution);
        if fresh || changed || record.status.phase == "review" {
            ensure!(
                request
                    .distribution
                    .as_ref()
                    .is_none_or(|name| !name.is_empty()),
                "Choose a distribution"
            );
            let mut inspected = record.clone();
            if let Some(name) = &request.distribution {
                validate_distribution(name)?;
                inspected.status.distribution = name.clone();
                if name != &record.status.distribution {
                    inspected.status.linux_user = None;
                }
            }
            if let Some(target) = selected_target(&record, &request, &targets, fresh) {
                inspected.status.distribution = target.distribution.clone();
                inspected.status.linux_user = Some(target.linux_user.clone());
            }
            let report = executor.execute("inspect", &inspected).await?;
            if changed {
                ensure!(
                    !record.status.existing_worker
                        || request
                            .engine
                            .is_none_or(|engine| Some(engine) == record.status.engine)
                        || request
                            .distribution
                            .as_ref()
                            .is_some_and(|name| name != &record.status.distribution),
                    "This configured worker already uses another engine. Preserve it, or migrate its engine explicitly after stopping work; setup will not silently switch its runtime."
                );
                record.status.revision = new_id();
                record.approved = false;
                record.status.phase = "review".into();
                record.status.error = None;
                record.status.manual_action = None;
                record.status.can_retry = false;
                for step in &mut record.status.steps {
                    step.state = "pending".into();
                }
            }
            let previous_plan = serde_json::json!([
                record.status.engine,
                record.status.distribution,
                record.status.linux_user,
                record.status.docker_terms_required,
                record.status.existing_worker
            ]);
            let compatible = report.compatible && report.detected_engine.is_some();
            plan(&mut record, report, &request, &targets, fresh)?;
            let next_plan = serde_json::json!([
                record.status.engine,
                record.status.distribution,
                record.status.linux_user,
                record.status.docker_terms_required,
                record.status.existing_worker
            ]);
            if !fresh && previous_plan != next_plan {
                record.status.revision = new_id();
                record.approved = false;
            }
            if compatible
                && record.status.existing_worker
                && record.status.phase == "review"
                && let Some(target) = record.existing_target.as_ref()
                && check_managed_worker(target).await.is_ok()
            {
                record.status.phase = "ready".into();
                record.status.message =
                    "The existing managed worker and its container runtime are ready.".into();
            }
        }
        // Only an explicitly approved reboot transition resumes automatically; ordinary refresh
        // never starts provisioning or revives a manually stopped worker.
        if record.status.phase == "restartRequired" && record.approved {
            let report = executor.execute("inspect", &record).await?;
            let rebooted = record
                .restart_boot_id
                .as_ref()
                .zip(report.boot_id.as_ref())
                .is_some_and(|(before, after)| before != after);
            if rebooted {
                record.status.can_restart = false;
                if report.wsl_ready {
                    return launch(settings_path, record, executor, lock);
                }
                finish_failure(
                    &mut record,
                    &anyhow::anyhow!(
                        "WSL2 is still unavailable after the Windows restart. Check hardware virtualization and Windows feature or organization restrictions, then retry. Your selected engine has been retained."
                    ),
                );
            }
        }
        save(&settings_path, &record)?;
        return Ok(record.status);
    }
    if request.action == "openDockerTerms" {
        executor.execute("openDockerTerms", &record).await?;
        return Ok(record.status);
    }
    if request.action == "openDocker" {
        ensure!(
            record.status.engine == Some(Engine::Desktop),
            "Docker Desktop is not selected"
        );
        executor.execute("openDocker", &record).await?;
        return Ok(record.status);
    }
    if request.action == "verify" {
        ensure!(
            record.status.phase == "actionRequired",
            "Verification is available after completing the indicated manual step; use Retry for failed setup"
        );
        ensure!(
            record.approved,
            "Review and start setup before verifying it"
        );
        record.status.manual_action = None;
        record.status.error = None;
        return launch(settings_path, record, executor, lock);
    }
    authorized(&record, &request)?;
    if request.action == "restart" {
        ensure!(
            record.status.phase == "restartRequired",
            "No setup restart is pending"
        );
        save(&settings_path, &record)?;
        executor.execute("restart", &record).await?;
        return Ok(record.status);
    }
    if request.action == "delete" {
        ensure!(
            record.status.owned_distribution && record.owned_registration.is_some(),
            "This distribution is not verified as Gridstator-owned"
        );
        for target in targets
            .iter()
            .filter(|target| target.distribution == record.status.distribution)
        {
            let mut query = bridge::worker_request("status");
            query["dataDir"] = serde_json::json!(target.data_dir);
            query["nodeId"] = serde_json::json!(target.node_id);
            query["caFingerprint"] = serde_json::json!(target.ca_fingerprint);
            let response =
                crate::wsl::execute(target, &query, std::time::Duration::from_secs(30)).await?;
            for worker in response["workers"]
                .as_array()
                .context("The worker status could not be verified before deletion")?
            {
                ensure!(
                    worker["running"] != true
                        && worker["draining"] != true
                        && worker["controllerRunning"] != true
                        && worker["activeJobs"].as_array().is_none_or(Vec::is_empty),
                    "Stop this distribution’s workers and controllers from the worker controls before deleting its data."
                );
            }
        }
        executor.execute("delete", &record).await?;
        record.status.owned_distribution = false;
        record.status.can_delete = false;
        record.status.existing_worker = false;
        record.owned_registration = None;
        record.approved = false;
        record.creation_requested = false;
        record.status.phase = "review".into();
        record.status.revision = new_id();
        record.status.message="The dedicated distribution and its data were removed. Shared Windows and Docker components were retained.".into();
        save(&settings_path, &record)?;
        return Ok(record.status);
    }
    ensure!(
        request.action == "start" || request.action == "retry",
        "Unsupported setup transition"
    );
    if record.status.phase == "restartRequired" {
        let report = executor.execute("inspect", &record).await?;
        ensure!(
            record
                .restart_boot_id
                .as_ref()
                .is_some_and(|boot| Some(boot) != report.boot_id.as_ref()),
            "Restart Windows before resuming this setup; its selected engine has been retained."
        );
    }
    if record.status.docker_terms_required {
        ensure!(
            request.accept_docker_terms == Some(true) || record.accepted_docker_terms,
            "Accept Docker’s terms in the review before installing Docker Desktop"
        );
        record.accepted_docker_terms = true;
        record.status.docker_terms_required = false;
    }
    record.approved = true;
    record.status.error = None;
    record.status.manual_action = None;
    launch(settings_path, record, executor, lock)
}

fn launch(
    settings_path: PathBuf,
    mut record: Record,
    executor: Arc<dyn Executor>,
    lock: fs::File,
) -> Result<Status> {
    record.status.phase = "running".into();
    record.status.can_retry = false;
    record.status.can_restart = false;
    record.status.message="Preparing the selected runtime. You can close this window; the background host will retain progress.".into();
    save(&settings_path, &record)?;
    let status = record.status.clone();
    tokio::spawn(async move {
        let _lock = lock;
        if let Err(error) = prepare(executor.as_ref(), &settings_path, &mut record).await {
            finish_failure(&mut record, &error);
            let _ = save(&settings_path, &record);
        }
    });
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        replies: std::sync::Mutex<VecDeque<(String, Result<Report>)>>,
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl Fake {
        fn new(replies: Vec<(&str, Result<Report>)>) -> Self {
            Self {
                replies: std::sync::Mutex::new(
                    replies.into_iter().map(|(a, r)| (a.into(), r)).collect(),
                ),
                calls: Default::default(),
            }
        }
    }
    impl Executor for Fake {
        fn execute<'a>(&'a self, action: &'a str, _: &'a Record) -> Task<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(action.into());
                let (expected, result) = self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected operation");
                assert_eq!(action, expected);
                result
            })
        }
    }
    fn request(action: &str) -> Request {
        Request {
            action: action.into(),
            revision: None,
            engine: None,
            distribution: None,
            confirmed: None,
            accept_docker_terms: None,
        }
    }
    #[test]
    fn defaults_preserve_initial_wsl_detection_and_working_engine_precedence() -> Result<()> {
        let mut r = initial();
        plan(&mut r, Report::default(), &request("inspect"), &[], true)?;
        assert_eq!(r.status.engine, Some(Engine::Desktop));
        plan(
            &mut r,
            Report {
                wsl_available: true,
                ..Report::default()
            },
            &request("inspect"),
            &[],
            false,
        )?;
        assert_eq!(
            r.status.engine,
            Some(Engine::Desktop),
            "installing WSL must not change the reviewed default"
        );
        let mut r = initial();
        plan(
            &mut r,
            Report {
                wsl_available: true,
                detected_engine: Some(Engine::Desktop),
                desktop_installed: true,
                ..Report::default()
            },
            &request("inspect"),
            &[],
            true,
        )?;
        assert_eq!(r.status.engine, Some(Engine::Desktop));
        assert!(!r.status.docker_terms_required);
        let mut r = initial();
        plan(
            &mut r,
            Report {
                wsl_available: true,
                ..Report::default()
            },
            &request("inspect"),
            &[],
            true,
        )?;
        assert_eq!(r.status.engine, Some(Engine::Engine));
        Ok(())
    }
    #[tokio::test]
    async fn failed_install_retries_same_plan_without_changing_engine() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut r = initial();
        r.status.engine = Some(Engine::Engine);
        let runner = Fake::new(vec![
            ("wsl", Ok(Report::default())),
            (
                "distribution",
                Ok(Report {
                    owned: true,
                    registration: Some("owned-guid".into()),
                    ..Report::default()
                }),
            ),
            ("runtime", Err(anyhow::anyhow!("download failed"))),
        ]);
        let error = prepare(&runner, &path, &mut r).await.unwrap_err();
        finish_failure(&mut r, &error);
        save(&path, &r)?;
        let mut resumed = load(&path)?.unwrap();
        assert_eq!(resumed.status.phase, "failed");
        assert_eq!(resumed.status.engine, Some(Engine::Engine));
        assert_eq!(resumed.owned_registration.as_deref(), Some("owned-guid"));
        let retry = Fake::new(vec![
            ("wsl", Ok(Report::default())),
            ("distribution", Ok(Report::default())),
            ("runtime", Ok(Report::default())),
            ("verify", Ok(Report::default())),
        ]);
        prepare(&retry, &path, &mut resumed).await?;
        assert_eq!(load(&path)?.unwrap().status.phase, "ready");
        Ok(())
    }
    #[tokio::test]
    async fn reboot_records_progress_before_request_and_retains_desktop_choice() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut r = initial();
        r.status.engine = Some(Engine::Desktop);
        r.approved = true;
        let runner = Fake::new(vec![
            (
                "wsl",
                Ok(Report {
                    restart_required: true,
                    ..Report::default()
                }),
            ),
            ("registerResume", Ok(Report::default())),
        ]);
        prepare(&runner, &path, &mut r).await?;
        let saved = load(&path)?.unwrap();
        assert_eq!(saved.status.phase, "restartRequired");
        assert_eq!(saved.status.engine, Some(Engine::Desktop));
        assert!(saved.approved);
        assert_eq!(saved.status.steps[0].state, "running");
        Ok(())
    }
    #[tokio::test]
    async fn stale_review_and_missing_consent_cannot_execute_side_effects() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut r = initial();
        r.status.engine = Some(Engine::Desktop);
        r.status.docker_terms_required = true;
        save(&path, &r)?;
        let runner = Arc::new(Fake::new(vec![]));
        let mut request = request("start");
        request.confirmed = Some(true);
        request.revision = Some("old".into());
        assert!(
            execute_with(path.clone(), request.clone(), vec![], runner.clone(), true)
                .await
                .unwrap_err()
                .to_string()
                .contains("plan changed")
        );
        request.revision = Some(r.status.revision);
        assert!(
            execute_with(path, request, vec![], runner.clone(), true)
                .await
                .unwrap_err()
                .to_string()
                .contains("terms")
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        Ok(())
    }
    #[tokio::test]
    async fn manual_step_keeps_plan_and_does_not_advance_to_ready() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut r = initial();
        r.status.engine = Some(Engine::Desktop);
        let runner = Fake::new(vec![
            ("wsl", Ok(Report::default())),
            ("distribution", Ok(Report::default())),
            (
                "runtime",
                Ok(Report {
                    action_required: Some("Enable integration".into()),
                    ..Report::default()
                }),
            ),
        ]);
        prepare(&runner, &path, &mut r).await?;
        assert_eq!(r.status.phase, "actionRequired");
        assert_eq!(
            r.status.manual_action.as_deref(),
            Some("Enable integration")
        );
        assert!(prepared_target(&path).is_err());
        Ok(())
    }
    #[test]
    fn unsupported_arbitrary_distribution_names_are_rejected() {
        for name in [
            "",
            "--all",
            "Ubuntu\nOther",
            "Ubuntu;Stop-Process",
            "C:\\Other",
            "$(run)",
        ] {
            assert!(validate_distribution(name).is_err(), "{name}");
        }
        assert!(validate_distribution("Ubuntu Work").is_ok());
    }

    #[tokio::test]
    async fn pending_restart_does_not_resume_before_a_new_windows_boot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.approved = true;
        record.status.phase = "restartRequired".into();
        record.status.engine = Some(Engine::Desktop);
        record.restart_boot_id = Some("boot-one".into());
        save(&path, &record)?;
        let runner = Arc::new(Fake::new(vec![(
            "inspect",
            Ok(Report {
                wsl_available: true,
                wsl_ready: true,
                boot_id: Some("boot-one".into()),
                ..Report::default()
            }),
        )]));
        let status = execute_with(path, request("inspect"), vec![], runner.clone(), true).await?;
        assert_eq!(status.phase, "restartRequired");
        assert_eq!(*runner.calls.lock().unwrap(), vec!["inspect"]);
        Ok(())
    }

    #[tokio::test]
    async fn approved_setup_resumes_after_reboot_with_same_engine() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.approved = true;
        record.status.phase = "restartRequired".into();
        record.status.engine = Some(Engine::Desktop);
        record.restart_boot_id = Some("boot-one".into());
        save(&path, &record)?;
        let runner = Arc::new(Fake::new(vec![
            (
                "inspect",
                Ok(Report {
                    wsl_available: true,
                    wsl_ready: true,
                    boot_id: Some("boot-two".into()),
                    ..Report::default()
                }),
            ),
            ("wsl", Ok(Report::default())),
            ("distribution", Ok(Report::default())),
            ("runtime", Ok(Report::default())),
            ("verify", Ok(Report::default())),
        ]));
        assert_eq!(
            execute_with(path.clone(), request("inspect"), vec![], runner, true)
                .await?
                .phase,
            "running"
        );
        for _ in 0..100 {
            if load(&path)?.unwrap().status.phase == "ready" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let saved = load(&path)?.unwrap();
        assert_eq!(saved.status.phase, "ready");
        assert_eq!(saved.status.engine, Some(Engine::Desktop));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_requests_cannot_start_duplicate_installers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.status.phase = "running".into();
        save(&path, &record)?;
        let _lock = acquire(&path)?.unwrap();
        let runner = Arc::new(Fake::new(vec![]));
        assert_eq!(
            execute_with(
                path.clone(),
                request("inspect"),
                vec![],
                runner.clone(),
                true
            )
            .await?
            .phase,
            "running"
        );
        assert!(
            execute_with(path, request("start"), vec![], runner.clone(), true)
                .await
                .is_err()
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn failed_owned_distribution_can_be_explicitly_deleted_without_shared_cleanup()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.status.phase = "failed".into();
        record.status.owned_distribution = true;
        record.status.can_delete = true;
        record.owned_registration = Some("owned-registration".into());
        save(&path, &record)?;
        let runner = Arc::new(Fake::new(vec![("delete", Ok(Report::default()))]));
        let mut request = request("delete");
        request.confirmed = Some(true);
        request.revision = Some(record.status.revision);
        let status = execute_with(path, request, vec![], runner.clone(), true).await?;
        assert_eq!(status.phase, "review");
        assert!(!status.owned_distribution);
        assert_eq!(*runner.calls.lock().unwrap(), vec!["delete"]);
        Ok(())
    }

    #[test]
    fn embedded_windows_helper_fits_the_windows_process_argument_limit() {
        let script = include_str!("../windows/prerequisites.ps1");
        // Include the extra escaping Windows adds to quotes and trailing backslashes.
        let escaped = script.encode_utf16().count()
            + script.chars().filter(|c| *c == '"' || *c == '\\').count()
            + 1024;
        assert!(
            escaped < 32_767,
            "embedded helper must fit CreateProcessW: {escaped}"
        );
    }
    #[tokio::test]
    async fn explicit_new_distribution_survives_engine_changes_and_reopening() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let target: Target = serde_json::from_value(serde_json::json!({
            "distribution":"Ubuntu", "registration":"old-guid", "linuxUser":"user",
            "cliPath":"/home/user/.local/bin/gridstator", "dataDir":"/home/user/agent",
            "nodeId":"existing-node", "clusterId":null, "caFingerprint":"pinned",
            "startAtSignIn":true, "cached":{}
        }))?;
        let targets = vec![target];
        let runner = Arc::new(Fake::new(vec![
            (
                "inspect",
                Ok(Report {
                    wsl_available: true,
                    ..Report::default()
                }),
            ),
            (
                "inspect",
                Ok(Report {
                    wsl_available: true,
                    ..Report::default()
                }),
            ),
            (
                "inspect",
                Ok(Report {
                    wsl_available: true,
                    ..Report::default()
                }),
            ),
            (
                "inspect",
                Ok(Report {
                    wsl_available: true,
                    ..Report::default()
                }),
            ),
        ]));
        let initial = execute_with(
            path.clone(),
            request("inspect"),
            targets.clone(),
            runner.clone(),
            true,
        )
        .await?;
        assert_eq!(initial.distribution, "Ubuntu");
        let mut choose = request("inspect");
        choose.distribution = Some(DEDICATED.into());
        execute_with(path.clone(), choose, targets.clone(), runner.clone(), true).await?;
        let mut choose_engine = request("inspect");
        choose_engine.engine = Some(Engine::Desktop);
        let changed = execute_with(
            path.clone(),
            choose_engine,
            targets.clone(),
            runner.clone(),
            true,
        )
        .await?;
        assert_eq!(changed.distribution, DEDICATED);
        assert!(!changed.existing_worker);
        let reopened = execute_with(path, request("inspect"), targets, runner, true).await?;
        assert_eq!(reopened.distribution, DEDICATED);
        assert_eq!(reopened.engine, Some(Engine::Desktop));
        Ok(())
    }
    #[tokio::test]
    async fn unavailable_wsl_after_reboot_becomes_actionable_failure() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.approved = true;
        record.status.phase = "restartRequired".into();
        record.status.engine = Some(Engine::Desktop);
        record.restart_boot_id = Some("before".into());
        save(&path, &record)?;
        let runner = Arc::new(Fake::new(vec![(
            "inspect",
            Ok(Report {
                boot_id: Some("after".into()),
                wsl_ready: false,
                ..Report::default()
            }),
        )]));
        let status = execute_with(path, request("inspect"), vec![], runner, true).await?;
        assert_eq!(status.phase, "failed");
        assert!(status.can_retry);
        assert!(!status.can_restart);
        assert_eq!(status.engine, Some(Engine::Desktop));
        assert!(status.error.unwrap().contains("virtualization"));
        Ok(())
    }
    #[tokio::test]
    async fn manual_verification_cannot_skip_required_reboot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("settings.json");
        let mut record = initial();
        record.approved = true;
        record.status.phase = "restartRequired".into();
        save(&path, &record)?;
        let runner = Arc::new(Fake::new(vec![]));
        assert!(
            execute_with(path, request("verify"), vec![], runner.clone(), true)
                .await
                .is_err()
        );
        assert!(runner.calls.lock().unwrap().is_empty());
        Ok(())
    }
    #[tokio::test]
    async fn desktop_login_startup_requires_reviewed_plan_and_worker_opt_in() -> Result<()> {
        let mut record = initial();
        record.approved = true;
        record.status.phase = "ready".into();
        record.status.engine = Some(Engine::Desktop);
        let mut target: Target = serde_json::from_value(serde_json::json!({
            "distribution":DEDICATED, "registration":"owned-guid", "linuxUser":ACCOUNT,
            "cliPath":"/home/gridstator/.local/bin/gridstator", "dataDir":"/home/gridstator/agent",
            "nodeId":"node", "clusterId":null, "caFingerprint":"pinned",
            "startAtSignIn":false, "cached":{}
        }))?;
        let runner = Fake::new(vec![("openDocker", Ok(Report::default()))]);
        start_login_runtime(&runner, &record, &[target.clone()]).await?;
        assert!(runner.calls.lock().unwrap().is_empty());
        target.start_at_sign_in = true;
        start_login_runtime(&runner, &record, &[target.clone()]).await?;
        record.status.engine = Some(Engine::Engine);
        start_login_runtime(&runner, &record, &[target.clone()]).await?;
        record.status.engine = Some(Engine::Desktop);
        record.approved = false;
        start_login_runtime(&runner, &record, &[target]).await?;
        assert_eq!(*runner.calls.lock().unwrap(), vec!["openDocker"]);
        Ok(())
    }
}
