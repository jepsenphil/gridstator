//! Real external-tool executors. Docker/BuildKit/act stay external; arguments never contain
//! secret values. The worker owns durable logging, redaction, result upload and acknowledgment.
use crate::workspace::{BlobStore, Manifest};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod actions;
mod build;
mod command;
mod process;
pub use actions::{ActLogParser, rewrite_local_checkouts};
pub use command::plan_environment;

pub const LABEL_MANAGED: &str = "io.gridstator.managed";
pub const LABEL_JOB: &str = "io.gridstator.job";
pub const LABEL_NODE: &str = "io.gridstator.node";
pub const LABEL_ASSIGNMENT: &str = "io.gridstator.assignment";

#[cfg(test)]
fn test_context(spec: Value, work_dir: PathBuf) -> ExecutionContext {
    ExecutionContext {
        job_id: "J".into(),
        assignment_id: "A".into(),
        attempt: 1,
        node_id: "N".into(),
        node_name: "test".into(),
        spec,
        blobs: BlobStore::new(work_dir.join("cas")),
        work_dir,
        manifest: None,
        keep_failed: false,
        act_bin: None,
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub job_id: String,
    pub assignment_id: String,
    pub attempt: u64,
    pub node_id: String,
    pub node_name: String,
    pub spec: Value,
    pub work_dir: PathBuf,
    pub manifest: Option<Manifest>,
    pub blobs: BlobStore,
    /// Preserve failed command containers/volumes for inspection until agent restart cleanup.
    pub keep_failed: bool,
    /// Exact resolved managed/PATH override chosen by the worker's capability probe.
    pub act_bin: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactFile {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResult {
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub oom_killed: bool,
    pub outputs: Value,
    pub artifacts: Vec<ArtifactFile>,
}

impl ExecutionResult {
    fn success() -> Self {
        Self {
            outcome: "succeeded".into(),
            exit_code: Some(0),
            error: None,
            oom_killed: false,
            outputs: Value::Null,
            artifacts: Vec::new(),
        }
    }
    fn failed(error: impl Into<String>, exit_code: Option<i32>) -> Self {
        Self {
            outcome: "failed".into(),
            exit_code,
            error: Some(error.into()),
            oom_killed: false,
            outputs: Value::Null,
            artifacts: Vec::new(),
        }
    }
    fn cancelled(exit_code: Option<i32>) -> Self {
        Self {
            outcome: "cancelled".into(),
            exit_code,
            error: None,
            oom_killed: false,
            outputs: Value::Null,
            artifacts: Vec::new(),
        }
    }
}

pub fn docker_binary() -> String {
    std::env::var("GRIDSTATOR_DOCKER").unwrap_or_else(|_| "docker".into())
}
fn act_binary() -> String {
    std::env::var("GRIDSTATOR_ACT").unwrap_or_else(|_| "act".into())
}

pub async fn execute(
    ctx: ExecutionContext,
    cancel: CancellationToken,
    logs: mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    if cancel.is_cancelled() {
        return Ok(ExecutionResult::cancelled(None));
    }
    if !ctx
        .job_id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        || ctx.job_id.is_empty()
    {
        bail!("Job identity cannot be used as a Docker resource name");
    }
    private_dir(&ctx.work_dir).await?;
    let result = match ctx.spec["kind"].as_str().unwrap_or("command") {
        "command" => command::execute(&ctx, &cancel, &logs).await,
        "docker-build" => build::execute(&ctx, &cancel, &logs).await,
        "actions" => actions::execute(&ctx, &cancel, &logs).await,
        kind => {
            return Ok(ExecutionResult::failed(
                format!("Unknown executor kind {kind}"),
                None,
            ));
        }
    };
    match result {
        Ok(result) => Ok(result),
        Err(error) if cancel.is_cancelled() => {
            system(&logs, &format!("Cancelled during setup: {error:#}")).await;
            Ok(ExecutionResult::cancelled(None))
        }
        Err(error) => Ok(ExecutionResult::failed(format!("{error:#}"), None)),
    }
}

async fn emit(logs: &mpsc::Sender<(String, String)>, stream: &str, text: String) {
    let _ = logs.send((stream.to_owned(), text)).await;
}
async fn system(logs: &mpsc::Sender<(String, String)>, text: &str) {
    emit(
        logs,
        "system",
        if text.ends_with('\n') {
            text.to_owned()
        } else {
            format!("{text}\n")
        },
    )
    .await;
}

fn string_map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}
fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn labels(ctx: &ExecutionContext) -> Vec<String> {
    vec![
        format!("{LABEL_MANAGED}=true"),
        format!("{LABEL_JOB}={}", ctx.job_id),
        format!("{LABEL_NODE}={}", ctx.node_id),
        format!("{LABEL_ASSIGNMENT}={}", ctx.assignment_id),
    ]
}
fn timeout(ctx: &ExecutionContext) -> Option<u64> {
    ctx.spec["timeoutSeconds"].as_u64()
}

async fn private_dir(path: &Path) -> Result<()> {
    tokio::fs::create_dir_all(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}
async fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    Ok(())
}

async fn docker_output(args: &[String]) -> Result<std::process::Output> {
    Ok(tokio::time::timeout(
        std::time::Duration::from_secs(60),
        Command::new(docker_binary())
            .args(args)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .context("Docker operation timed out")??)
}
async fn docker_check(args: &[String]) -> Result<std::process::Output> {
    let output = docker_output(args).await?;
    if !output.status.success() {
        bail!(
            "docker {} failed: {}",
            args.first().map_or("", String::as_str),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

/// Resolve a requested build/workflow path after materialization, including symlink traversal.
async fn resolve_inside(root: &Path, path: &str, directory: bool) -> Result<PathBuf> {
    if path != "." {
        crate::workspace::check_snapshot_path(path)?;
    }
    let root = tokio::fs::canonicalize(root).await?;
    let resolved = tokio::fs::canonicalize(root.join(path))
        .await
        .with_context(|| format!("Path {path} does not exist in the snapshot"))?;
    if !resolved.starts_with(&root) {
        bail!("Path {path} resolves outside the snapshot");
    }
    let metadata = tokio::fs::metadata(&resolved).await?;
    if directory && !metadata.is_dir() || !directory && !metadata.is_file() {
        bail!("Path {path} has the wrong file type");
    }
    Ok(resolved)
}

async fn stage(ctx: &ExecutionContext) -> Result<PathBuf> {
    let manifest = ctx
        .manifest
        .clone()
        .context("This job requires a source snapshot")?;
    let store = ctx.blobs.clone();
    let destination = ctx.work_dir.join("src");
    let target = destination.clone();
    tokio::task::spawn_blocking(move || crate::workspace::materialize(&manifest, &store, &target))
        .await??;
    Ok(destination)
}

/// Cleanup is always selected by managed+node ownership labels, optionally by job. Shared cache
/// volumes are never removed. Startup may keep active job IDs; all other jobs belong to this node.
pub async fn cleanup_managed(node_id: &str, keep_jobs: &HashSet<String>) -> Result<usize> {
    cleanup_resources(node_id, None, keep_jobs).await
}

async fn cleanup_job(ctx: &ExecutionContext) -> Result<usize> {
    cleanup_resources(&ctx.node_id, Some(&ctx.job_id), &HashSet::new()).await
}

async fn cleanup_resources(
    node_id: &str,
    job_id: Option<&str>,
    keep: &HashSet<String>,
) -> Result<usize> {
    let mut filters = vec![
        "--filter".into(),
        format!("label={LABEL_MANAGED}=true"),
        "--filter".into(),
        format!("label={LABEL_NODE}={node_id}"),
    ];
    if let Some(job) = job_id {
        filters.extend(["--filter".into(), format!("label={LABEL_JOB}={job}")]);
    }
    let mut count = 0;
    for resource in ["container", "volume"] {
        let format = if resource == "container" {
            "{{.ID}}"
        } else {
            "{{.Name}}"
        };
        let mut args = vec![resource.into(), "ls".into()];
        if resource == "container" {
            args.push("-a".into());
        }
        args.extend(filters.clone());
        args.extend(["--format".into(), format.into()]);
        let output = docker_check(&args).await?;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let id = line.trim();
            if id.is_empty() {
                continue;
            }
            let label_format = if resource == "container" {
                "{{json .Config.Labels}}"
            } else {
                "{{json .Labels}}"
            };
            let inspected = docker_check(&[
                resource.into(),
                "inspect".into(),
                "--format".into(),
                label_format.into(),
                id.into(),
            ])
            .await?;
            let ownership: Value = serde_json::from_slice(&inspected.stdout)?;
            let job = ownership[LABEL_JOB].as_str().unwrap_or("");
            if ownership[LABEL_MANAGED] != "true"
                || ownership[LABEL_NODE] != node_id
                || ownership["io.gridstator.cache"] == "true"
                || job.is_empty()
                || keep.contains(job)
            {
                continue;
            }
            let args = vec![resource.into(), "rm".into(), "-f".into(), id.into()];
            if docker_check(&args).await.is_ok() {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn stopped_result(
    stop: Option<process::StopReason>,
    exit: Option<i32>,
    timeout: Option<u64>,
    tool: &str,
) -> ExecutionResult {
    match stop {
        Some(process::StopReason::Cancelled) => ExecutionResult::cancelled(exit),
        Some(process::StopReason::Timeout) => {
            ExecutionResult::failed(format!("Timed out after {}s", timeout.unwrap_or(0)), exit)
        }
        None if exit == Some(0) => ExecutionResult::success(),
        None => ExecutionResult::failed(
            format!(
                "{tool} exited with code {}",
                exit.map_or("unknown".into(), |code| code.to_string())
            ),
            exit,
        ),
    }
}

#[cfg(all(test, unix))]
mod path_tests {
    use super::*;

    #[tokio::test]
    async fn build_and_workflow_path_resolution_rejects_symlink_escape() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("ctx")).unwrap();
        for (path, directory) in [("ctx", true), ("ctx/Dockerfile", false)] {
            let error = resolve_inside(root.path(), path, directory)
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("resolves outside the snapshot"),
                "{error:#}"
            );
        }
    }
}
