//! Versioned private-stdin operations for the Node-based VS Code extension.
use crate::{client::Client, discovery, state, workspace};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub const CAPABILITIES: &[&str] = &[
    "bridge-v1",
    "native-controller",
    "native-worker",
    "snapshots",
    "release-verification",
    "local-setup-v1",
    "worker-control-v1",
];
fn project(start: &Path) -> Result<(PathBuf, String)> {
    let start = std::fs::canonicalize(start)?;
    let git_root = git_output(&start, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let mut directory = start.clone();
    let mut configured = None;
    loop {
        if directory.join("gridstator.yml").exists() || directory.join("gridstator.yaml").exists() {
            configured = Some(directory.clone());
            break;
        }
        if git_root.as_ref() == Some(&directory) {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent.into();
    }
    let root = configured.or(git_root).unwrap_or_else(|| start.clone());
    let relative = start
        .strip_prefix(&root)
        .context("workspace outside project root")?;
    let workdir = if relative.as_os_str().is_empty() {
        ".".into()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };
    Ok((root, workdir))
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
fn snapshot_options(
    snapshot: &Value,
    overrides: Option<&Value>,
) -> Result<workspace::CaptureOptions> {
    let mut merged = snapshot.clone();
    if !merged.is_object() {
        merged = json!({});
    }
    // Project numbers are MiB; the private helper accepts an explicit byte count.
    if let Some(maximum) = merged.get("maxFileSize") {
        let bytes = if let Some(number) = maximum.as_u64() {
            number
                .checked_mul(1024 * 1024)
                .context("snapshot maximum overflow")?
        } else {
            let size = maximum.as_str().context("invalid snapshot maximum")?;
            let pattern = regex::Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*([a-z]*)\s*$")?;
            let parts = pattern.captures(size).context("invalid snapshot size")?;
            let exponent = match parts[2].to_ascii_lowercase().as_str() {
                "" | "b" => 0,
                "k" | "kb" | "ki" | "kib" => 1,
                "m" | "mb" | "mi" | "mib" => 2,
                "g" | "gb" | "gi" | "gib" => 3,
                "t" | "tb" | "ti" | "tib" => 4,
                _ => anyhow::bail!("invalid snapshot size unit"),
            };
            let value = (parts[1].parse::<f64>()? * 1024f64.powi(exponent)).round();
            ensure!(
                value.is_finite() && value >= 0.0 && value < u64::MAX as f64,
                "snapshot maximum overflow"
            );
            value as u64
        };
        merged["maxFileSize"] = json!(bytes);
    }
    if let Some(overrides) = overrides {
        for (key, value) in overrides
            .as_object()
            .context("snapshotOptions must be an object")?
        {
            ensure!(
                ["include", "exclude", "allowSecrets", "maxFileSize", "noGit"]
                    .contains(&key.as_str()),
                "unsupported snapshot option {key}"
            );
            merged[key] = value.clone();
        }
    }
    let mut options: workspace::CaptureOptions = serde_json::from_value(merged)?;
    options.cache_dir = Some(
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("gridstator-v2/snapshots"),
    );
    Ok(options)
}
async fn check_cluster(client: &Client, request: &Value) -> Result<()> {
    if let Some(expected) = request.get("expectedClusterId") {
        let expected = expected
            .as_str()
            .context("expectedClusterId must be a string")?;
        let cluster = client.get("/v1/cluster").await?;
        ensure!(
            cluster["clusterId"].as_str() == Some(expected),
            "controller changed; reconnect before submitting work"
        );
    }
    Ok(())
}
pub async fn execute(request: Value) -> Result<Value> {
    ensure!(request["version"] == 1, "unsupported native bridge version");
    let operation = request["operation"]
        .as_str()
        .context("bridge operation required")?;
    let root = request["root"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    match operation {
        "worker" => crate::local_worker::execute(request).await,
        "workerHost" => crate::local_worker::host(&request).await,
        "joinWorker" => crate::cli::join_local_worker(&request).await,
        "discover" => {
            tokio::task::spawn_blocking(move || {
                let (root, workdir) = project(&root)?;
                let mut result = discovery::discover(&root)?;
                result["workdir"] = json!(workdir);
                Ok(result)
            })
            .await?
        }
        "capture" | "submit" => {
            let config_path = request["configPath"].as_str().map(PathBuf::from);
            let client = Client::load(config_path.as_deref())?;
            check_cluster(&client, &request).await?;
            let (root, _) = tokio::task::spawn_blocking(move || project(&root)).await??;
            let root_copy = root.clone();
            let options = request.get("snapshotOptions").cloned();
            let captured = Arc::new(
                tokio::task::spawn_blocking(move || {
                    let discovered = discovery::discover(&root_copy)?;
                    let snapshot = &discovered["config"]["snapshot"];
                    let options = snapshot_options(snapshot, options.as_ref())?;
                    workspace::capture(&root_copy, &options)
                })
                .await??,
            );
            crate::transfer::upload_capture(&client, captured.clone()).await?;
            if operation == "capture" {
                return Ok(
                    json!({"id":captured.id,"stats":captured.stats,"warnings":captured.warnings}),
                );
            }
            let mut spec = request["spec"].clone();
            ensure!(spec.is_object(), "submit spec required");
            if spec["kind"] == "actions" && spec.get("git").is_none() {
                spec["git"] = tokio::task::spawn_blocking(move || {
                    let mut context = json!({});
                    for (key, args) in [
                        ("branch", vec!["rev-parse", "--abbrev-ref", "HEAD"]),
                        ("remoteUrl", vec!["remote", "get-url", "origin"]),
                        ("headSha", vec!["rev-parse", "HEAD"]),
                    ] {
                        if let Some(value) = git_output(&root, &args) {
                            context[key] = json!(value);
                        }
                    }
                    context
                })
                .await?;
            }
            spec["snapshot"] = json!(captured.id);
            let spec = crate::protocol::normalize_spec(spec)?;
            let shards = request["shards"].as_u64().unwrap_or(1);
            ensure!((1..=64).contains(&shards), "shards must be 1..64");
            check_cluster(&client, &request).await?;
            if shards > 1 {
                client
                    .post("/v1/groups", json!({"spec":spec,"shards":shards}))
                    .await
            } else {
                client.post("/v1/jobs", json!({"spec":spec})).await
            }
        }
        "setup" => {
            let config_path = request["configPath"]
                .as_str()
                .map(PathBuf::from)
                .context("setup configPath required")?;
            let port = request
                .get("port")
                .map(|port| {
                    port.as_u64()
                        .filter(|port| (1..=u16::MAX as u64).contains(port))
                        .map(|port| port as u16)
                        .context("setup port must be 1..65535")
                })
                .transpose()?;
            crate::cli::setup_local_controller(&config_path, port).await
        }
        "local" => {
            let args = request["args"]
                .as_array()
                .context("local args required")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .context("local argument must be string")
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                args.len() <= 64 && args.iter().all(|s| s.len() < 8192),
                "local argument limits exceeded"
            );
            ensure!(
                args.first()
                    .is_some_and(|s| ["status", "ensure-login", "service", "configure"]
                        .contains(&s.as_str())),
                "unsupported local operation"
            );
            let mut command = tokio::process::Command::new(std::env::current_exe()?);
            command
                .args(["controller", "local"])
                .args(args)
                .arg("--json")
                .kill_on_drop(true);
            if let Some(config) = request["configPath"].as_str() {
                command.env("GRIDSTATOR_CONFIG", config);
            }
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(120), command.output())
                    .await??;
            ensure!(
                output.status.success(),
                "local administration failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok(serde_json::from_slice(&output.stdout)?)
        }
        "paths" => Ok(
            json!({"configPath":state::config_path(),"controllerData":state::data_dir("controller"),"agentData":state::data_dir("agent")}),
        ),
        _ => anyhow::bail!("unsupported bridge operation {operation}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_limits_use_project_units_and_explicit_overrides() -> Result<()> {
        let config = json!({"include":["ignored"],"exclude":["tmp"],"allowSecrets":[".env.test"],"maxFileSize":2});
        let options = snapshot_options(&config, None)?;
        assert_eq!(options.max_file_size, 2 * 1024 * 1024);
        let options = snapshot_options(&config, Some(&json!({"include":[],"maxFileSize":7})))?;
        assert!(options.include.is_empty());
        assert_eq!(options.exclude, vec!["tmp"]);
        assert_eq!(options.max_file_size, 7);
        assert_eq!(
            snapshot_options(&json!({"maxFileSize":"1.5MiB"}), None)?.max_file_size,
            1572864
        );
        assert!(snapshot_options(&config, Some(&json!({"cacheDir":"/untrusted"}))).is_err());
        Ok(())
    }
    #[test]
    fn project_discovery_stops_at_git_root() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("gridstator.yml"), "version: 1\n")?;
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo)?;
        ensure!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(&repo)
                .status()?
                .success(),
            "git init"
        );
        let subdir = repo.join("sub");
        std::fs::create_dir(&subdir)?;
        assert_eq!(
            project(&subdir)?,
            (std::fs::canonicalize(&repo)?, "sub".into())
        );
        std::fs::write(subdir.join("gridstator.yaml"), "version: 1\n")?;
        assert_eq!(
            project(&subdir)?,
            (std::fs::canonicalize(&subdir)?, ".".into())
        );
        Ok(())
    }
}
