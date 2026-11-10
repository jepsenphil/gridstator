use super::process::{self, Run, StopAction};
use super::*;
use std::collections::BTreeMap;
use std::io::Write;

pub struct EnvironmentPlan {
    pub file_content: String,
    pub pass_through: BTreeMap<String, String>,
}

pub fn reserved_env(name: &str, build: bool) -> bool {
    [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "TMPDIR",
        "TEMP",
        "TMP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
    ]
    .contains(&name)
        || name.starts_with("DOCKER_")
        || name.starts_with("BUILDKIT_")
        || build && name.starts_with("BUILDX_")
}

/// Docker's env-file cannot represent line breaks. Such values are passed by name with the
/// secret in the CLI process environment, never as `--env NAME=value` arguments.
pub fn plan_environment(spec: &Value, ctx: &ExecutionContext) -> Result<EnvironmentPlan> {
    let mut values = BTreeMap::from([
        ("CI".into(), "true".into()),
        ("GRIDSTATOR".into(), "1".into()),
        ("GRIDSTATOR_JOB_ID".into(), ctx.job_id.clone()),
        ("GRIDSTATOR_ATTEMPT".into(), ctx.attempt.to_string()),
        ("GRIDSTATOR_NODE_ID".into(), ctx.node_id.clone()),
        ("GRIDSTATOR_NODE_NAME".into(), ctx.node_name.clone()),
        ("npm_config_cache".into(), "/gridstator/cache/npm".into()),
        (
            "npm_config_store_dir".into(),
            "/gridstator/cache/pnpm-store".into(),
        ),
        ("YARN_CACHE_FOLDER".into(), "/gridstator/cache/yarn".into()),
        ("COREPACK_HOME".into(), "/gridstator/cache/corepack".into()),
        ("COREPACK_ENABLE_DOWNLOAD_PROMPT".into(), "0".into()),
        ("PIP_CACHE_DIR".into(), "/gridstator/cache/pip".into()),
        ("GOMODCACHE".into(), "/gridstator/cache/go/mod".into()),
        ("GOCACHE".into(), "/gridstator/cache/go/build".into()),
    ]);
    values.extend(string_map(&spec["env"]));
    values.extend(string_map(&spec["secretEnv"]));
    let mut plan = EnvironmentPlan {
        file_content: String::new(),
        pass_through: BTreeMap::new(),
    };
    for (name, value) in values {
        if value.contains(['\r', '\n']) {
            if reserved_env(&name, false) {
                bail!("Environment variable {name} cannot contain line breaks");
            }
            plan.pass_through.insert(name, value);
        } else {
            plan.file_content.push_str(&format!("{name}={value}\n"));
        }
    }
    Ok(plan)
}

pub async fn execute(
    ctx: &ExecutionContext,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    // Do not delete by a name collision. Every removal goes through managed+node+job labels.
    cleanup_job(ctx).await?;
    let result = run(ctx, cancel, logs).await;
    let failed = match &result {
        Ok(result) => result.outcome == "failed",
        Err(_) => true,
    };
    if ctx.keep_failed && !cancel.is_cancelled() && failed {
        system(
            logs,
            "Keeping failed container and workspace volume for inspection",
        )
        .await;
    } else if let Err(error) = cleanup_job(ctx).await {
        system(
            logs,
            &format!("Could not remove owned job resources: {error:#}"),
        )
        .await;
    }
    result
}

async fn run(
    ctx: &ExecutionContext,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    let name = format!("gridstator-{}", ctx.job_id.to_lowercase());
    let volume = format!("gridstator-ws-{}", ctx.job_id.to_lowercase());
    let mut create_volume = vec!["volume".into(), "create".into()];
    for label in labels(ctx) {
        create_volume.extend(["--label".into(), label]);
    }
    create_volume.push(volume.clone());
    docker_check(&create_volume).await?;
    let ownership = docker_check(&[
        "volume".into(),
        "inspect".into(),
        "--format".into(),
        "{{json .Labels}}".into(),
        volume.clone(),
    ])
    .await?;
    let ownership: Value = serde_json::from_slice(&ownership.stdout)?;
    if ownership[LABEL_MANAGED] != "true"
        || ownership[LABEL_NODE] != ctx.node_id
        || ownership[LABEL_JOB] != ctx.job_id
    {
        bail!("Workspace volume already exists without matching ownership labels");
    }
    docker_check(&[
        "volume".into(),
        "create".into(),
        "--label".into(),
        format!("{LABEL_MANAGED}=true"),
        "--label".into(),
        "io.gridstator.cache=true".into(),
        "gridstator-cache".into(),
    ])
    .await?;
    if cancel.is_cancelled() {
        return Ok(ExecutionResult::cancelled(None));
    }
    let env_file = ctx.work_dir.join("container.env");
    let plan = plan_environment(&ctx.spec, ctx)?;
    private_file(&env_file, plan.file_content.as_bytes()).await?;
    let workdir = ctx.spec["workdir"].as_str().unwrap_or(".");
    if workdir != "." {
        crate::workspace::check_snapshot_path(workdir)?;
    }
    let mut args = vec![
        "create".into(),
        "--name".into(),
        name.clone(),
        "--init".into(),
        "--pull".into(),
        "missing".into(),
        "--cpus".into(),
        ctx.spec["resources"]["cpus"]
            .as_f64()
            .unwrap_or(2.0)
            .to_string(),
        "--memory".into(),
        format!(
            "{}m",
            ctx.spec["resources"]["memoryMb"].as_u64().unwrap_or(4096)
        ),
        "--memory-swap".into(),
        format!(
            "{}m",
            ctx.spec["resources"]["memoryMb"].as_u64().unwrap_or(4096)
        ),
        "--pids-limit".into(),
        "4096".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--mount".into(),
        format!("type=volume,src={volume},dst=/workspace"),
        "--mount".into(),
        "type=volume,src=gridstator-cache,dst=/gridstator/cache".into(),
        "--workdir".into(),
        if workdir == "." {
            "/workspace".into()
        } else {
            format!("/workspace/{workdir}")
        },
        "--env-file".into(),
        env_file.to_string_lossy().into_owned(),
    ];
    for label in labels(ctx) {
        args.extend(["--label".into(), label]);
    }
    for key in plan.pass_through.keys() {
        args.extend(["--env".into(), key.clone()]);
    }
    args.push(
        ctx.spec["image"]
            .as_str()
            .unwrap_or("node:24-bookworm")
            .into(),
    );
    args.extend(string_array(&ctx.spec["command"]));
    system(logs, &format!("Creating container on {}", ctx.node_name)).await;
    let created = process::run(
        Run {
            bin: &docker_binary(),
            args: &args,
            env: &plan.pass_through,
            cwd: None,
            timeout: None,
            phase_running: false,
            stop_action: StopAction::Interrupt(10),
            act_parser: None,
            output_mode: process::OutputMode::Setup,
        },
        cancel,
        logs,
    )
    .await;
    let _ = tokio::fs::remove_file(&env_file).await;
    let created = created?;
    if created.stop.is_some() {
        return Ok(stopped_result(
            created.stop,
            created.exit_code,
            None,
            "docker create",
        ));
    }
    if created.exit_code != Some(0) {
        return Ok(ExecutionResult::failed(
            "Could not create container; see Docker output",
            None,
        ));
    }
    if cancel.is_cancelled() {
        return Ok(ExecutionResult::cancelled(None));
    }
    if let Some(manifest) = ctx.manifest.clone() {
        system(logs, "Copying workspace snapshot into the container").await;
        let source = ctx.work_dir.join("workspace.tar");
        let output = source.clone();
        let blobs = ctx.blobs.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            crate::workspace::workspace_tar(&manifest, &blobs, &mut file)?;
            file.flush()?;
            Ok(())
        })
        .await??;
        let stdin = std::fs::File::open(&source)?;
        let mut command = Command::new(docker_binary());
        command
            .args(["cp", "-", &format!("{name}:/workspace")])
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn()?;
        let copied = tokio::select! { output = child.wait_with_output() => Some(output?), _ = cancel.cancelled() => None };
        let _ = tokio::fs::remove_file(&source).await;
        let Some(copied) = copied else {
            return Ok(ExecutionResult::cancelled(None));
        };
        if !copied.status.success() {
            bail!(
                "Could not copy workspace: {}",
                String::from_utf8_lossy(&copied.stderr)
            );
        }
    }
    if cancel.is_cancelled() {
        return Ok(ExecutionResult::cancelled(None));
    }
    let args = vec!["start".into(), "--attach".into(), name.clone()];
    let outcome = process::run(
        Run {
            bin: &docker_binary(),
            args: &args,
            env: &BTreeMap::new(),
            cwd: None,
            timeout: timeout(ctx),
            phase_running: true,
            stop_action: StopAction::Container(name.clone()),
            act_parser: None,
            output_mode: process::OutputMode::Job,
        },
        cancel,
        logs,
    )
    .await?;
    let state = docker_check(&[
        "inspect".into(),
        "--format".into(),
        "{{json .State}}".into(),
        name.clone(),
    ])
    .await?;
    let state: Value = serde_json::from_slice(&state.stdout)?;
    let exit = state["ExitCode"].as_i64().map(|code| code as i32);
    let mut result = stopped_result(outcome.stop, exit, timeout(ctx), "Process");
    if outcome.stop.is_none() && state["OOMKilled"] == true {
        result = ExecutionResult::failed("Container was killed because it ran out of memory", exit);
        result.oom_killed = true;
    } else if outcome.stop.is_none()
        && result.outcome == "failed"
        && state["Error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    {
        result.error = state["Error"].as_str().map(str::to_owned);
    }
    if result.outcome != "cancelled" {
        let artifact_dir = ctx.work_dir.join("artifacts");
        private_dir(&artifact_dir).await?;
        for (index, path) in string_array(&ctx.spec["artifacts"]).iter().enumerate() {
            if path != "." {
                crate::workspace::check_snapshot_path(path)?;
            }
            let destination = artifact_dir.join(format!("{index}.tar"));
            let mut file = std::fs::OpenOptions::new();
            file.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                file.mode(0o600);
            }
            let artifact = file.open(&destination)?;
            let source = if path == "." {
                format!("{name}:/workspace")
            } else {
                format!("{name}:/workspace/{path}")
            };
            let output = Command::new(docker_binary())
                .args(["cp", &source, "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::from(artifact))
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?
                .wait_with_output()
                .await?;
            if output.status.success() {
                result.artifacts.push(ArtifactFile {
                    name: format!("{}.tar", if path == "." { "workspace" } else { path }),
                    path: destination,
                });
            } else {
                let _ = tokio::fs::remove_file(destination).await;
                system(
                    logs,
                    &format!("Artifact {path}: not found in the workspace"),
                )
                .await;
            }
        }
    }
    Ok(result)
}
