use super::process::{self, Run, StopAction};
use super::*;
use serde_yaml_ng::Value as Yaml;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub struct ActLogParser {
    steps: Vec<Value>,
    jobs: BTreeMap<String, String>,
    changed: bool,
}

impl ActLogParser {
    pub fn steps(&self) -> Vec<Value> {
        self.steps.clone()
    }
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
    pub fn failed_jobs(&self) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|(_, state)| state.as_str() != "success")
            .map(|(job, _)| job.clone())
            .collect()
    }
    pub fn finish(&mut self, cancelled: bool) {
        self.finish_after_stop(
            cancelled,
            cancelled.then(|| chrono::Utc::now().timestamp_millis()),
        );
    }
    fn finish_after_stop(&mut self, stopped: bool, cancelled_at: Option<i64>) {
        for step in &mut self.steps {
            let ended_at = step["startedAt"]
                .as_i64()
                .map(|started| started + step["durationMs"].as_i64().unwrap_or(0));
            let interrupted_failure = step["status"] == "failure"
                && cancelled_at
                    .is_some_and(|since| ended_at.is_none_or(|ended| ended >= since - 1000));
            if step["status"] == "running" || interrupted_failure {
                step["status"] = json!(if stopped { "cancelled" } else { "skipped" });
            }
        }
        self.changed = true;
    }
    pub fn feed(&mut self, line: &str, origin: &str) -> Vec<(String, String)> {
        let line = line.trim_end_matches('\n');
        if line.trim().is_empty() {
            return Vec::new();
        }
        let Ok(entry) = serde_json::from_str::<Value>(line.trim()) else {
            return vec![(origin.into(), format!("{line}\n"))];
        };
        if !entry.is_object() {
            return vec![(origin.into(), format!("{line}\n"))];
        }
        let msg = entry["msg"].as_str().unwrap_or("");
        let debug = matches!(entry["level"].as_str(), Some("debug" | "trace"));
        let Some(job) = entry["job"].as_str().filter(|job| !job.is_empty()) else {
            if debug {
                return Vec::new();
            }
            let text = if let Some(error) = entry["error"].as_str() {
                format!("{msg}: {error}")
            } else {
                msg.to_owned()
            };
            return if text.is_empty() {
                Vec::new()
            } else {
                vec![("system".into(), format!("act: {text}\n"))]
            };
        };
        if let Some(result) = entry["jobResult"].as_str() {
            self.jobs.insert(job.into(), result.into());
            self.changed = true;
        }
        let step_id = entry
            .get("stepID")
            .filter(|value| !value.is_null())
            .or_else(|| entry.get("stepid"))
            .and_then(Value::as_array);
        if let Some(ids) = step_id.filter(|ids| !ids.is_empty())
            && let Some(name) = entry["step"].as_str().filter(|name| !name.is_empty())
        {
            let stage = entry["stage"].as_str().unwrap_or("");
            let id = format!(
                "{job}#{stage}#{}",
                ids.iter()
                    .map(|id| id
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| id.to_string()))
                    .collect::<Vec<_>>()
                    .join(".")
            );
            let name = if !stage.is_empty() && stage != "Main" {
                format!("{stage} {name}")
            } else {
                name.into()
            };
            let index = if let Some(index) = self.steps.iter().position(|step| step["id"] == id) {
                index
            } else {
                self.steps.push(json!({"id":id,"group":job,"name":name,"status":"running","startedAt":null,"durationMs":null}));
                self.changed = true;
                self.steps.len() - 1
            };
            let step = &mut self.steps[index];
            let now = chrono::Utc::now().timestamp_millis();
            if msg.contains("⭐ Run ") && step["startedAt"].is_null() {
                step["startedAt"] = json!(now);
                self.changed = true;
            }
            if let Some(status) = entry["stepResult"].as_str() {
                step["status"] = json!(if ["success", "failure", "skipped", "cancelled"]
                    .contains(&status)
                {
                    status
                } else {
                    "failure"
                });
                step["durationMs"] = if let Some(nanos) = entry["executionTime"].as_f64() {
                    json!((nanos / 1e6).round().max(0.0) as u64)
                } else if let Some(start) = step["startedAt"].as_i64() {
                    json!((now - start).max(0))
                } else {
                    Value::Null
                };
                self.changed = true;
            }
        }
        if entry["raw_output"] == true {
            return msg
                .strip_suffix('\n')
                .unwrap_or(msg)
                .split('\n')
                .map(|line| ("stdout".into(), format!("[{job}]   | {line}\n")))
                .collect();
        }
        if debug || msg.trim().is_empty() {
            return Vec::new();
        }
        let text = if let Some(error) = entry["error"].as_str() {
            format!("{}: {error}", msg.trim())
        } else {
            msg.trim().into()
        };
        vec![(
            if entry["level"] == "error" {
                "stderr"
            } else {
                "stdout"
            }
            .into(),
            format!("[{job}] {text}\n"),
        )]
    }
}

fn repo_slug(remote: Option<&str>) -> Option<String> {
    let remote = remote?
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let (owner, repo) = remote.rsplit_once('/')?;
    let owner = owner.rsplit(['/', ':']).next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn yaml_text(value: &Yaml) -> Option<String> {
    match value {
        Yaml::String(value) => Some(value.clone()),
        Yaml::Number(value) => Some(value.to_string()),
        Yaml::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn rewrite(document: &mut Yaml, slug: Option<&str>) -> Vec<Value> {
    let Some(jobs) = document.get_mut("jobs").and_then(Yaml::as_mapping_mut) else {
        return Vec::new();
    };
    let mut rewrites = Vec::new();
    for (job_id, job) in jobs {
        let job_id = yaml_text(job_id).unwrap_or_else(|| "?".into());
        let Some(steps) = job.get_mut("steps").and_then(Yaml::as_sequence_mut) else {
            continue;
        };
        for (index, step) in steps.iter_mut().enumerate() {
            let Some(uses) = step.get("uses").and_then(Yaml::as_str).map(str::to_owned) else {
                continue;
            };
            if !uses.trim().starts_with("actions/checkout@") {
                continue;
            }
            let name = step
                .get("name")
                .and_then(yaml_text)
                .unwrap_or_else(|| format!("#{} ({uses})", index + 1));
            let Some(with) = step.get_mut("with").and_then(Yaml::as_mapping_mut) else {
                continue;
            };
            if let Some(repository) = with.get("repository").and_then(yaml_text) {
                let repository = repository.trim();
                let expression = repository.starts_with("${{")
                    && repository.ends_with("}}")
                    && repository[3..repository.len() - 2].trim() == "github.repository";
                if !expression && !slug.is_some_and(|slug| slug.eq_ignore_ascii_case(repository)) {
                    continue;
                }
            }
            let mut removed = serde_json::Map::new();
            for key in ["ref", "repository"] {
                if let Some(value) = with.remove(key) {
                    removed.insert(key.into(), json!(yaml_text(&value).unwrap_or_default()));
                }
            }
            if removed.is_empty() {
                continue;
            }
            if with.is_empty()
                && let Some(step) = step.as_mapping_mut()
            {
                step.remove("with");
            }
            rewrites.push(json!({"job":job_id,"step":name,"removed":removed}));
        }
    }
    rewrites
}

/// Rewrite only private workflow copies. Explicit refs for this repository must never replace
/// the submitted working tree; foreign repository checkouts retain their original behavior.
pub fn rewrite_local_checkouts(source: &str, slug: Option<&str>) -> Result<(String, Vec<Value>)> {
    let Ok(mut document) = serde_yaml_ng::from_str::<Yaml>(source) else {
        return Ok((source.into(), Vec::new()));
    };
    let rewrites = rewrite(&mut document, slug);
    let text = if rewrites.is_empty() {
        source.into()
    } else {
        serialize_workflow(&document, source)?
    };
    Ok((text, rewrites))
}

// serde_yaml retains YAML values, but not comment nodes. Keep the comments in a preamble of
// the private copy; the workflow in the submitted snapshot remains byte-for-byte unchanged.
fn serialize_workflow(document: &Yaml, source: &str) -> Result<String> {
    let mut comments = String::new();
    let mut quoted = None;
    for line in source.lines() {
        let mut chars = line.char_indices().peekable();
        let mut previous = None;
        while let Some((index, ch)) = chars.next() {
            match (quoted, ch) {
                (Some('"'), '\\') => {
                    chars.next();
                }
                (Some('\''), '\'') if chars.peek().is_some_and(|(_, ch)| *ch == '\'') => {
                    chars.next();
                }
                (Some(quote), ch) if quote == ch => quoted = None,
                (None, '\'' | '"')
                    if previous.is_none_or(char::is_whitespace)
                        || matches!(previous, Some(':' | '[' | '{' | ',')) =>
                {
                    quoted = Some(ch)
                }
                (None, '#') if previous.is_none_or(char::is_whitespace) => {
                    comments.push_str(&line[index..]);
                    comments.push('\n');
                    break;
                }
                _ => {}
            }
            previous = Some(ch);
        }
    }
    comments.push_str(&serde_yaml_ng::to_string(document)?);
    Ok(comments)
}

fn local_workflow_name(uses: &str, copied: &BTreeSet<String>) -> Option<String> {
    let uses = uses.trim();
    if !uses.starts_with("./") {
        return None;
    }
    let mut parts = Vec::new();
    for part in uses.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    if parts.len() == 3
        && parts[0] == ".github"
        && parts[1] == "workflows"
        && copied.contains(parts[2])
    {
        Some(parts[2].to_owned())
    } else {
        None
    }
}

async fn prepare_workflows(
    src: &Path,
    workflow: &Path,
    slug: Option<&str>,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<PathBuf> {
    let dir = src.join(".github/workflows");
    let mut listing = match tokio::fs::read_dir(&dir).await {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(workflow.to_owned());
        }
        Err(error) => return Err(error.into()),
    };
    let mut workflows = BTreeMap::new();
    let mut any_rewrites = false;
    while let Some(file) = listing.next_entry().await? {
        let name = file.file_name();
        let name = name.to_str().context("Workflow filename requires UTF-8")?;
        if !(name.ends_with(".yml") || name.ends_with(".yaml"))
            || !file.file_type().await?.is_file()
        {
            continue;
        }
        let source = tokio::fs::read_to_string(file.path()).await?;
        let (text, rewrites) = rewrite_local_checkouts(&source, slug)?;
        any_rewrites |= !rewrites.is_empty();
        for entry in rewrites {
            system(
                logs,
                &format!(
                    ".github/workflows/{name}: job {} checks out the submitted snapshot",
                    entry["job"].as_str().unwrap_or("?")
                ),
            )
            .await;
        }
        workflows.insert(name.to_owned(), text);
    }
    if !any_rewrites {
        return Ok(workflow.to_owned());
    }
    let copied: BTreeSet<_> = workflows.keys().cloned().collect();
    let destination = src.join(".git/gridstator/workflows");
    private_dir(&destination).await?;
    for (name, mut text) in workflows {
        if let Ok(mut document) = serde_yaml_ng::from_str::<Yaml>(&text) {
            let mut redirected = false;
            if let Some(jobs) = document.get_mut("jobs").and_then(Yaml::as_mapping_mut) {
                for (_, job) in jobs {
                    let Some(uses) = job.get("uses").and_then(Yaml::as_str) else {
                        continue;
                    };
                    let Some(name) = local_workflow_name(uses, &copied) else {
                        continue;
                    };
                    let replacement = format!("./.git/gridstator/workflows/{name}");
                    if let Some(job) = job.as_mapping_mut() {
                        job.insert(Yaml::String("uses".into()), Yaml::String(replacement));
                        redirected = true;
                    }
                }
            }
            if redirected {
                text = serialize_workflow(&document, &text)?;
            }
        }
        private_file(&destination.join(&name), text.as_bytes()).await?;
    }
    let name = workflow.file_name().context("Workflow has no filename")?;
    Ok(destination.join(name))
}

async fn git(src: &Path, home: &Path, args: &[String]) -> Result<()> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(src)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Gridstator")
        .env("GIT_AUTHOR_EMAIL", "gridstator@localhost")
        .env("GIT_COMMITTER_NAME", "Gridstator")
        .env("GIT_COMMITTER_EMAIL", "gridstator@localhost")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "Could not prepare git context for act: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn default_platforms() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "ubuntu-latest".into(),
            "catthehacker/ubuntu:act-latest".into(),
        ),
        (
            "ubuntu-24.04".into(),
            "catthehacker/ubuntu:act-24.04".into(),
        ),
        (
            "ubuntu-22.04".into(),
            "catthehacker/ubuntu:act-22.04".into(),
        ),
    ])
}

fn dotenv_line(name: &str, value: &str) -> Result<String> {
    Ok(format!("{name}={}\n", serde_json::to_string(value)?))
}

async fn restore_ownership(
    ctx: &ExecutionContext,
    src: &Path,
    platforms: &BTreeMap<String, String>,
) -> Result<()> {
    for image in platforms.values() {
        if !docker_output(&["image".into(), "inspect".into(), image.clone()])
            .await?
            .status
            .success()
        {
            continue;
        }
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--pull=never".into(),
            "--network=none".into(),
            "--read-only".into(),
            "--user=0:0".into(),
        ];
        for label in labels(ctx) {
            args.extend(["--label".into(), label]);
        }
        args.extend([
            "--mount".into(),
            format!("type=bind,src={},dst=/workspace", src.display()),
            "--entrypoint=sh".into(),
            image.clone(),
            "-c".into(),
            "chown -Rh \"$(stat -c \"%u:%g\" /workspace)\" /workspace".into(),
        ]);
        docker_check(&args).await?;
        return Ok(());
    }
    bail!("No cached runner image is available to restore snapshot ownership")
}

fn act_args(
    ctx: &ExecutionContext,
    src: &Path,
    workflow: &Path,
    home: &Path,
    private: &Path,
    artifact_dir: &Path,
    port: u16,
) -> Vec<String> {
    let mut args = vec![
        ctx.spec["event"].as_str().unwrap_or("push").into(),
        "-C".into(),
        src.to_string_lossy().into_owned(),
        "-W".into(),
        workflow.to_string_lossy().into_owned(),
        "--json".into(),
        "--rm".into(),
        "--pull=false".into(),
        "--secret-file".into(),
        private.join("secrets").to_string_lossy().into_owned(),
        "--var-file".into(),
        private.join("vars").to_string_lossy().into_owned(),
        "--env-file".into(),
        private.join("env").to_string_lossy().into_owned(),
        "--artifact-server-path".into(),
        artifact_dir.to_string_lossy().into_owned(),
        "--artifact-server-port".into(),
        port.to_string(),
        "--cache-server-path".into(),
        home.join("cache-server").to_string_lossy().into_owned(),
        "--cache-server-addr".into(),
        "127.0.0.1".into(),
        "--container-options".into(),
        labels(ctx)
            .iter()
            .map(|label| format!("--label {label}"))
            .collect::<Vec<_>>()
            .join(" "),
    ];
    if let Some(job) = ctx.spec["job"].as_str() {
        args.extend(["-j".into(), job.into()]);
    }
    let socket = ctx.spec["dockerSocket"] == true;
    if socket {
        args.push("--bind".into());
    } else {
        args.extend(["--container-daemon-socket".into(), "-".into()]);
    }
    let mut platforms = default_platforms();
    platforms.extend(string_map(&ctx.spec["platforms"]));
    for (label, image) in &platforms {
        args.extend(["-P".into(), format!("{label}={image}")]);
    }
    for (name, value) in string_map(&ctx.spec["inputs"]) {
        args.extend(["--input".into(), format!("{name}={value}")]);
    }
    for value in string_array(&ctx.spec["matrix"]) {
        args.extend(["--matrix".into(), value]);
    }
    if ctx.spec.get("eventPayload").is_some() {
        args.extend([
            "--eventpath".into(),
            private.join("event.json").to_string_lossy().into_owned(),
        ]);
    }
    args
}

pub async fn execute(
    ctx: &ExecutionContext,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    let src = stage(ctx).await?;
    let workflow = resolve_inside(
        &src,
        ctx.spec["workflow"]
            .as_str()
            .context("Workflow is required")?,
        false,
    )
    .await?;
    let home = ctx
        .work_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&ctx.work_dir)
        .join("act-home");
    private_dir(&home).await?;
    git(
        &src,
        &home,
        &[
            "init".into(),
            "-q".into(),
            "-b".into(),
            ctx.spec["git"]["branch"].as_str().unwrap_or("main").into(),
        ],
    )
    .await?;
    git(&src, &home, &["add".into(), "-A".into()]).await?;
    git(
        &src,
        &home,
        &[
            "commit".into(),
            "-q".into(),
            "--allow-empty".into(),
            "-m".into(),
            format!("Gridstator snapshot for job {}", ctx.job_id),
        ],
    )
    .await?;
    let remote = ctx.spec["git"]["remoteUrl"].as_str();
    if let Some(remote) = remote {
        git(
            &src,
            &home,
            &[
                "remote".into(),
                "add".into(),
                "origin".into(),
                remote.into(),
            ],
        )
        .await?;
    }
    let workflow = prepare_workflows(&src, &workflow, repo_slug(remote).as_deref(), logs).await?;
    let artifact_dir = ctx.work_dir.join("actions-artifacts");
    private_dir(&artifact_dir).await?;
    let private = ctx.work_dir.join("act-private");
    private_dir(&private).await?;
    let secret = private.join("secrets");
    let vars = private.join("vars");
    let env_file = private.join("env");
    for (path, values) in [
        (&secret, &ctx.spec["secretEnv"]),
        (&vars, &ctx.spec["vars"]),
        (&env_file, &ctx.spec["env"]),
    ] {
        let mut content = String::new();
        for (name, value) in string_map(values) {
            content.push_str(&dotenv_line(&name, &value)?);
        }
        private_file(path, content.as_bytes()).await?;
    }
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    if let Some(payload) = ctx.spec.get("eventPayload") {
        private_file(&private.join("event.json"), &serde_json::to_vec(payload)?).await?;
    }
    let args = act_args(ctx, &src, &workflow, &home, &private, &artifact_dir, port);
    let socket = ctx.spec["dockerSocket"] == true;
    let mut platforms = default_platforms();
    platforms.extend(string_map(&ctx.spec["platforms"]));
    let mut env = BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    if std::env::var_os("DOCKER_CONFIG").is_none()
        && let Some(config) = dirs::home_dir()
            .map(|home| home.join(".docker"))
            .filter(|path| path.is_dir())
    {
        env.insert(
            "DOCKER_CONFIG".into(),
            config.to_string_lossy().into_owned(),
        );
    }
    let parser = Arc::new(Mutex::new(ActLogParser::default()));
    let result = process::run(
        Run {
            bin: &ctx
                .act_bin
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(act_binary),
            args: &args,
            env: &env,
            cwd: Some(&src),
            timeout: timeout(ctx),
            phase_running: true,
            stop_action: StopAction::Interrupt(15),
            act_parser: Some(parser.clone()),
            output_mode: process::OutputMode::Job,
        },
        cancel,
        logs,
    )
    .await;
    let _ = tokio::fs::remove_dir_all(private).await;
    if let Err(error) = cleanup_job(ctx).await {
        system(
            logs,
            &format!("Could not remove workflow resources: {error:#}"),
        )
        .await;
    }
    if socket && let Err(error) = restore_ownership(ctx, &src, &platforms).await {
        system(
            logs,
            &format!("Could not restore staged snapshot ownership: {error:#}"),
        )
        .await;
    }
    let result = result?;
    let (steps, failures) = {
        let mut parser = parser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        parser.finish_after_stop(result.stop.is_some(), result.cancelled_at);
        (parser.steps(), parser.failed_jobs())
    };
    let mut outcome = stopped_result(result.stop, result.exit_code, timeout(ctx), "act");
    outcome.outputs = json!({"steps":steps});
    emit(logs, "outputs", outcome.outputs.to_string()).await;
    if result.stop.is_none() && outcome.outcome == "failed" && !failures.is_empty() {
        outcome.error = Some(format!("Workflow job(s) failed: {}", failures.join(", ")));
    }
    if result.stop.is_none() {
        let root = artifact_dir.clone();
        outcome.artifacts = tokio::task::spawn_blocking(move || -> Result<Vec<ArtifactFile>> {
            let mut files = Vec::new();
            let mut stack = vec![root.clone()];
            while let Some(directory) = stack.pop() {
                for entry in std::fs::read_dir(directory)? {
                    let entry = entry?;
                    let kind = entry.file_type()?;
                    if kind.is_dir() {
                        stack.push(entry.path());
                    } else if kind.is_file() {
                        let path = entry.path();
                        let relative = path
                            .strip_prefix(&root)?
                            .to_str()
                            .context("Artifact path requires UTF-8")?;
                        let name = relative
                            .split_once('/')
                            .map_or(relative, |(_, rest)| rest)
                            .to_owned();
                        crate::workspace::check_snapshot_path(&name)?;
                        files.push(ArtifactFile { name, path });
                    }
                }
            }
            files.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(files)
        })
        .await??;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const WORKFLOW: &str = r#"# keep this comment
name: "✅ Checks: Coverage"
on: { workflow_call: {} }
jobs:
  coverage:
    steps:
      - name: Checkout
        uses: actions/checkout@v4
        with:
          ref: ${{ inputs.source_ref || github.sha }}
          persist-credentials: false # preserve this too
      - uses: actions/checkout@v4
        with: { ref: main }
      - name: Same repo by name
        uses: actions/checkout@v4
        with: { repository: Acme/Widgets, ref: v1, path: sub }
      - name: Same repo by expression
        uses: actions/checkout@v4
        with: { repository: '${{ github.repository }}' }
      - name: Other repo
        uses: actions/checkout@v4
        with: { repository: acme/tools, ref: v2 }
      - name: Plain
        uses: actions/checkout@v4
      - name: Not checkout
        uses: actions/setup-node@v4
        with: { ref: x }
"#;

    #[test]
    fn source_checkout_remote_formats_and_exact_rewrite_contract() {
        for remote in [
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "git@github.com:acme/widgets.git",
            "ssh://git@github.com/acme/widgets.git",
        ] {
            assert_eq!(repo_slug(Some(remote)).as_deref(), Some("acme/widgets"));
        }
        assert_eq!(repo_slug(None), None);
        let (text, rewrites) = rewrite_local_checkouts(WORKFLOW, Some("acme/widgets")).unwrap();
        let doc: Yaml = serde_yaml_ng::from_str(&text).unwrap();
        let steps = doc["jobs"]["coverage"]["steps"].as_sequence().unwrap();
        let actual: Vec<_> = steps
            .iter()
            .map(|step| serde_json::to_value(&step["with"]).unwrap())
            .collect();
        assert_eq!(
            actual,
            vec![
                json!({"persist-credentials":false}),
                Value::Null,
                json!({"path":"sub"}),
                Value::Null,
                json!({"repository":"acme/tools","ref":"v2"}),
                Value::Null,
                json!({"ref":"x"})
            ]
        );
        assert!(text.contains("# keep this comment"));
        assert!(text.contains("# preserve this too"));
        assert_eq!(
            rewrites
                .iter()
                .map(|entry| entry["step"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "Checkout",
                "#2 (actions/checkout@v4)",
                "Same repo by name",
                "Same repo by expression"
            ]
        );
        assert_eq!(
            rewrites[0],
            json!({"job":"coverage","step":"Checkout","removed":{"ref":"${{ inputs.source_ref || github.sha }}"}})
        );
        let (_, unknown) = rewrite_local_checkouts(WORKFLOW, None).unwrap();
        assert!(
            !unknown
                .iter()
                .any(|entry| entry["step"] == "Same repo by name")
        );
        for source in [
            "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v4\n",
            "jobs: [",
        ] {
            assert_eq!(
                rewrite_local_checkouts(source, None).unwrap(),
                (source.into(), vec![])
            );
        }
    }

    #[tokio::test]
    async fn source_private_workflow_copies_preserve_originals_redirect_calls_and_invalid_yaml() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let caller = "name: Checks\non: push\njobs:\n  coverage:\n    uses: ./.github/workflows/coverage.yml\n  lint:\n    uses: './.github/workflows/sub/../lint.yml'\n  shared:\n    uses: acme/shared/.github/workflows/coverage.yml@v1\n";
        let coverage = "on: workflow_call\njobs:\n  run:\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          ref: '${{ github.sha }}'\n          persist-credentials: false\n";
        let lint =
            "on: workflow_call\njobs:\n  lint:\n    steps:\n      - uses: actions/checkout@v4\n";
        let files = [
            ("checks.yml", caller),
            ("coverage.yml", coverage),
            ("lint.yml", lint),
            ("broken.yml", "jobs: ["),
        ];
        for (name, text) in &files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        std::fs::create_dir(dir.join("directory.yml")).unwrap();
        let (send, mut receive) = mpsc::channel(16);
        let selected = prepare_workflows(
            root.path(),
            &dir.join("checks.yml"),
            Some("acme/widgets"),
            &send,
        )
        .await
        .unwrap();
        assert_eq!(
            selected,
            root.path().join(".git/gridstator/workflows/checks.yml")
        );
        for (name, text) in files {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), text);
        }
        let copy = root.path().join(".git/gridstator/workflows");
        let caller: Yaml =
            serde_yaml_ng::from_str(&std::fs::read_to_string(copy.join("checks.yml")).unwrap())
                .unwrap();
        assert_eq!(
            caller["jobs"]["coverage"]["uses"].as_str(),
            Some("./.git/gridstator/workflows/coverage.yml")
        );
        assert_eq!(
            caller["jobs"]["lint"]["uses"].as_str(),
            Some("./.git/gridstator/workflows/lint.yml")
        );
        assert_eq!(
            caller["jobs"]["shared"]["uses"].as_str(),
            Some("acme/shared/.github/workflows/coverage.yml@v1")
        );
        let rewritten: Yaml =
            serde_yaml_ng::from_str(&std::fs::read_to_string(copy.join("coverage.yml")).unwrap())
                .unwrap();
        assert_eq!(
            serde_json::to_value(&rewritten["jobs"]["run"]["steps"]).unwrap(),
            json!([{"uses":"actions/checkout@v4","with":{"persist-credentials":false}}])
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("lint.yml")).unwrap(),
            lint
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("broken.yml")).unwrap(),
            "jobs: ["
        );
        assert!(!copy.join("directory.yml").exists());
        assert!(
            receive
                .try_recv()
                .unwrap()
                .1
                .contains("coverage.yml: job run")
        );
    }

    #[tokio::test]
    async fn source_workflow_preparation_writes_nothing_without_rewrites_or_directory() {
        let root = TempDir::new().unwrap();
        let workflow = root.path().join(".github/workflows/lint.yml");
        let (send, _) = mpsc::channel(16);
        assert_eq!(
            prepare_workflows(root.path(), &workflow, None, &send)
                .await
                .unwrap(),
            workflow
        );
        std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        std::fs::write(
            &workflow,
            "on: push\njobs:\n  lint:\n    steps:\n      - uses: actions/checkout@v4\n",
        )
        .unwrap();
        assert_eq!(
            prepare_workflows(root.path(), &workflow, None, &send)
                .await
                .unwrap(),
            workflow
        );
        assert!(!root.path().join(".git").exists());
    }

    #[test]
    fn source_act_arguments_lock_defaults_and_keep_secret_values_out_of_argv() {
        let ctx = test_context(
            json!({"kind":"actions","event":"workflow_dispatch","job":"ok","inputs":{"who":"me"},"secretEnv":{"S":"secret-value"},"platforms":{"ubuntu-latest":"node:24-bookworm-slim"}}),
            PathBuf::from("/work"),
        );
        let args = act_args(
            &ctx,
            Path::new("/s"),
            Path::new("/s/w.yml"),
            Path::new("/home"),
            Path::new("/private"),
            Path::new("/a"),
            40000,
        )
        .join(" ");
        for expected in [
            "--container-daemon-socket -",
            "--secret-file /private/secrets",
            "--cache-server-addr 127.0.0.1",
            "--label io.gridstator.job=J",
            "-j ok",
            "--input who=me",
            "-P ubuntu-latest=node:24-bookworm-slim",
        ] {
            assert!(args.contains(expected), "{args}");
        }
        assert!(!args.contains("--no-skip-checkout"));
        assert!(!args.contains("secret-value"));
        assert_eq!(dotenv_line("K", "a\nb\"c").unwrap(), "K=\"a\\nb\\\"c\"\n");
    }

    #[test]
    fn source_act_log_console_steps_failures_and_change_tracking() {
        let mut parser = ActLogParser::default();
        let entries = [
            json!({"msg":"Using docker host 'unix:///var/run/docker.sock'"}),
            json!({"job":"CI/test","msg":"⭐ Run Set up job","step":"Set up job","stepid":["--setup-job"]}),
            json!({"job":"CI/test","msg":"  ✅  Success - Set up job","step":"Set up job","stepResult":"success","stepid":["--setup-job"],"executionTime":0}),
            json!({"job":"CI/test","msg":"⭐ Run Main Test","stage":"Main","step":"Test","stepID":["1"]}),
            json!({"job":"CI/test","msg":"line one\nline two\n","raw_output":true,"stage":"Main","step":"Test","stepID":["1"]}),
            json!({"job":"CI/test","msg":"  ❌  Failure - Main Test","stage":"Main","step":"Test","stepID":["1"],"stepResult":"failure","executionTime":2_500_000_000u64}),
            json!({"job":"CI/test","msg":"🏁  Job failed","jobResult":"failure"}),
            json!({"job":"CI/test","level":"debug","msg":"noise"}),
        ];
        let rendered: Vec<_> = entries
            .iter()
            .flat_map(|entry| parser.feed(&entry.to_string(), "stdout"))
            .collect();
        assert_eq!(
            rendered,
            [
                (
                    "system".into(),
                    "act: Using docker host 'unix:///var/run/docker.sock'\n".into()
                ),
                ("stdout".into(), "[CI/test] ⭐ Run Set up job\n".into()),
                (
                    "stdout".into(),
                    "[CI/test] ✅  Success - Set up job\n".into()
                ),
                ("stdout".into(), "[CI/test] ⭐ Run Main Test\n".into()),
                ("stdout".into(), "[CI/test]   | line one\n".into()),
                ("stdout".into(), "[CI/test]   | line two\n".into()),
                (
                    "stdout".into(),
                    "[CI/test] ❌  Failure - Main Test\n".into()
                ),
                ("stdout".into(), "[CI/test] 🏁  Job failed\n".into())
            ]
        );
        assert_eq!(
            parser
                .steps()
                .iter()
                .map(|step| (
                    step["group"].clone(),
                    step["name"].clone(),
                    step["status"].clone(),
                    step["durationMs"].clone()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    json!("CI/test"),
                    json!("Set up job"),
                    json!("success"),
                    json!(0)
                ),
                (
                    json!("CI/test"),
                    json!("Test"),
                    json!("failure"),
                    json!(2500)
                )
            ]
        );
        assert_eq!(parser.failed_jobs(), ["CI/test"]);
        assert!(parser.take_changed());
        assert!(!parser.take_changed());
    }

    #[test]
    fn source_act_log_passthrough_post_stage_finish_and_cancelled_failure() {
        let mut parser = ActLogParser::default();
        assert_eq!(
            parser.feed("plain text", "stderr"),
            [("stderr".into(), "plain text\n".into())]
        );
        parser.feed(&json!({"job":"W/j","msg":"⭐ Run Post Cache","stage":"Post","step":"Cache","stepID":["2"]}).to_string(), "stdout");
        assert_eq!(parser.steps()[0]["name"], "Post Cache");
        parser.finish(false);
        assert_eq!(parser.steps()[0]["status"], "skipped");
        let mut parser = ActLogParser::default();
        parser.feed(&json!({"job":"W/j","msg":"⭐ Run Main Long","stage":"Main","step":"Long","stepID":["0"]}).to_string(), "stdout");
        parser.feed(&json!({"job":"W/j","msg":"x","stage":"Main","step":"Long","stepID":["0"],"stepResult":"failure","executionTime":5_000_000_000u64}).to_string(), "stdout");
        parser.finish(true);
        assert_eq!(parser.steps()[0]["status"], "cancelled");
    }
}
