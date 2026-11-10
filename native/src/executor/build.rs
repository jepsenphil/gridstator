use super::command::reserved_env;
use super::process::{self, Run, StopAction};
use super::*;

pub async fn execute(
    ctx: &ExecutionContext,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    let result = run(ctx, cancel, logs).await;
    // Only the job-specific temporary tag is removed; user-requested image tags persist.
    if ctx.spec["output"] == "tar" && string_array(&ctx.spec["tags"]).is_empty() {
        let tag = export_tag(ctx);
        if let Ok(image) = docker_output(&[
            "image".into(),
            "inspect".into(),
            "--format".into(),
            "{{json .Config.Labels}}".into(),
            tag.clone(),
        ])
        .await
            && image.status.success()
            && let Ok(labels) = serde_json::from_slice::<Value>(&image.stdout)
            && labels[LABEL_MANAGED] == "true"
            && labels[LABEL_JOB] == ctx.job_id
            && labels[LABEL_NODE] == ctx.node_id
        {
            let _ = docker_output(&["image".into(), "rm".into(), tag]).await;
        }
    }
    result
}

fn export_tag(ctx: &ExecutionContext) -> String {
    format!("gridstator-export/{}:latest", ctx.job_id.to_lowercase())
}

fn build_args(
    ctx: &ExecutionContext,
    context: &Path,
    dockerfile: &Path,
    metadata: &Path,
) -> Result<Vec<String>> {
    let output_mode = ctx.spec["output"].as_str().unwrap_or("load");
    let tags = string_array(&ctx.spec["tags"]);
    let mut args = vec![
        "buildx".into(),
        "build".into(),
        "--progress=plain".into(),
        "--file".into(),
        dockerfile.to_string_lossy().into_owned(),
        "--metadata-file".into(),
        metadata.to_string_lossy().into_owned(),
    ];
    for label in labels(ctx) {
        args.extend(["--label".into(), label]);
    }
    if let Some(stage) = ctx.spec["stage"].as_str() {
        args.extend(["--target".into(), stage.into()]);
    }
    for (name, value) in string_map(&ctx.spec["buildArgs"]) {
        args.extend(["--build-arg".into(), format!("{name}={value}")]);
    }
    for name in string_map(&ctx.spec["secretEnv"]).keys() {
        args.extend(["--secret".into(), format!("id={name},env={name}")]);
    }
    for tag in &tags {
        args.extend(["--tag".into(), tag.clone()]);
    }
    let platforms = string_array(&ctx.spec["platforms"]);
    if !platforms.is_empty() {
        args.extend(["--platform".into(), platforms.join(",")]);
    }
    if ctx.spec["noCache"] == true {
        args.push("--no-cache".into());
    }
    if ctx.spec["pull"] == true {
        args.push("--pull".into());
    }
    for value in string_array(&ctx.spec["cacheFrom"]) {
        args.extend(["--cache-from".into(), value]);
    }
    for value in string_array(&ctx.spec["cacheTo"]) {
        args.extend(["--cache-to".into(), value]);
    }
    match output_mode {
        "load" => args.push("--load".into()),
        "push" => args.push("--push".into()),
        "tar" => {
            args.push("--load".into());
            if tags.is_empty() {
                args.extend(["--tag".into(), export_tag(ctx)]);
            }
        }
        "none" => args.extend(["--output".into(), "type=cacheonly".into()]),
        _ => bail!("Unknown build output {output_mode}"),
    }
    args.push(context.to_string_lossy().into_owned());
    Ok(args)
}

async fn run(
    ctx: &ExecutionContext,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ExecutionResult> {
    let output_mode = ctx.spec["output"].as_str().unwrap_or("load");
    let tags = string_array(&ctx.spec["tags"]);
    if output_mode == "push" && tags.is_empty() {
        bail!("output \"push\" needs at least one --tag");
    }
    let secrets = string_map(&ctx.spec["secretEnv"]);
    for name in secrets.keys() {
        if reserved_env(name, true) {
            bail!("Build secret name {name} is reserved; choose another name");
        }
    }
    system(logs, "Staging build context from the snapshot").await;
    let src = stage(ctx).await?;
    let relative_context = ctx.spec["context"].as_str().unwrap_or(".");
    let context = resolve_inside(&src, relative_context, true).await?;
    let default_file = if relative_context == "." {
        "Dockerfile".into()
    } else {
        format!("{relative_context}/Dockerfile")
    };
    let relative_dockerfile = ctx.spec["dockerfile"].as_str().unwrap_or(&default_file);
    let dockerfile = resolve_inside(&src, relative_dockerfile, false)
        .await
        .with_context(|| {
            format!("Dockerfile {relative_dockerfile:?} does not exist or cannot be used")
        })?;
    if cancel.is_cancelled() {
        return Ok(ExecutionResult::cancelled(None));
    }
    let metadata = ctx.work_dir.join("metadata.json");
    let args = build_args(ctx, &context, &dockerfile, &metadata)?;
    let mut env = secrets;
    env.insert("BUILDX_NO_DEFAULT_ATTESTATIONS".into(), "1".into());
    let built = process::run(
        Run {
            bin: &docker_binary(),
            args: &args,
            env: &env,
            cwd: None,
            timeout: timeout(ctx),
            phase_running: true,
            stop_action: StopAction::Interrupt(10),
            act_parser: None,
            output_mode: process::OutputMode::Job,
        },
        cancel,
        logs,
    )
    .await?;
    let mut result = stopped_result(
        built.stop,
        built.exit_code,
        timeout(ctx),
        "docker buildx build",
    );
    if result.outcome != "succeeded" {
        return Ok(result);
    }
    let metadata: Value = tokio::fs::read(&metadata)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}));
    let (id, size) = resolve_image(&metadata, output_mode, inspect_image_size).await;
    result.outputs = json!({"image":{"id":id,"digest":metadata["containerimage.digest"].as_str(),"tags":tags,"sizeBytes":size,"output":output_mode}});
    emit(logs, "outputs", result.outputs.to_string()).await;
    if output_mode == "tar" {
        let destination = ctx.work_dir.join("image.tar");
        let refs = if tags.is_empty() {
            vec![export_tag(ctx)]
        } else {
            tags
        };
        let mut save = vec![
            "save".into(),
            "--output".into(),
            destination.to_string_lossy().into_owned(),
        ];
        save.extend(refs);
        let saved = process::run(
            Run {
                bin: &docker_binary(),
                args: &save,
                env: &BTreeMap::new(),
                cwd: None,
                timeout: None,
                phase_running: false,
                stop_action: StopAction::Interrupt(10),
                act_parser: None,
                output_mode: process::OutputMode::Job,
            },
            cancel,
            logs,
        )
        .await?;
        if saved.stop.is_some() || saved.exit_code != Some(0) {
            return Ok(stopped_result(
                saved.stop,
                saved.exit_code,
                None,
                "docker save",
            ));
        }
        result.artifacts.push(ArtifactFile {
            name: "image.tar".into(),
            path: destination,
        });
    }
    Ok(result)
}

async fn inspect_image_size(id: &str) -> Option<u64> {
    if let Ok(description) = docker_output(&[
        "image".into(),
        "inspect".into(),
        "--format".into(),
        "{{.Size}}".into(),
        id.into(),
    ])
    .await
        && description.status.success()
        && let Ok(value) = String::from_utf8_lossy(&description.stdout)
            .trim()
            .parse::<u64>()
    {
        return Some(value);
    }
    None
}

async fn resolve_image<'a, F, Fut>(
    metadata: &'a Value,
    output_mode: &str,
    mut inspect: F,
) -> (Option<&'a str>, Option<u64>)
where
    F: FnMut(&'a str) -> Fut,
    Fut: std::future::Future<Output = Option<u64>>,
{
    let id = image_id(metadata, output_mode);
    if matches!(output_mode, "load" | "tar") {
        let mut previous = None;
        for candidate in [id, metadata["containerimage.digest"].as_str()]
            .into_iter()
            .flatten()
        {
            if previous == Some(candidate) {
                continue;
            }
            previous = Some(candidate);
            if let Some(size) = inspect(candidate).await {
                return (Some(candidate), Some(size));
            }
        }
    }
    // External image removal must not turn a successful build into an executor failure.
    (id, None)
}

// Containerd can address loaded images only by manifest/index digest, even when Buildx
// metadata also includes a config digest. Prefer config when it is locally inspectable.
// A remote/cache-only build does not establish a locally inspectable image ID.
fn image_id<'a>(metadata: &'a Value, output_mode: &str) -> Option<&'a str> {
    metadata["containerimage.config.digest"]
        .as_str()
        .or_else(|| {
            if matches!(output_mode, "load" | "tar") {
                metadata["containerimage.digest"].as_str()
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_image_resolution_uses_addressable_digest_when_config_is_not_inspectable() {
        let capture: Value = serde_json::from_str(include_str!(
            "../../contracts/buildx-containerd-linux-arm-inspect.json"
        ))
        .unwrap();
        let metadata = &capture["metadata"];
        assert_eq!(
            capture["candidateInspections"]["containerimage.config.digest"]["exitCode"],
            1
        );
        assert_eq!(
            capture["candidateInspections"]["containerimage.digest"]["exitCode"],
            0
        );
        for mode in ["load", "tar"] {
            let mut calls = Vec::new();
            let (id, size) = resolve_image(metadata, mode, |candidate| {
                calls.push(candidate.to_owned());
                let key = if Some(candidate) == metadata["containerimage.config.digest"].as_str() {
                    "containerimage.config.digest"
                } else {
                    assert_eq!(Some(candidate), metadata["containerimage.digest"].as_str());
                    "containerimage.digest"
                };
                let response = &capture["candidateInspections"][key];
                let size = if response["exitCode"] == 0 {
                    let image: Value =
                        serde_json::from_str(response["stdout"].as_str().unwrap()).unwrap();
                    image[0]["Size"].as_u64()
                } else {
                    None
                };
                std::future::ready(size)
            })
            .await;
            assert_eq!(id, capture["tagImage"]["Id"].as_str());
            assert_eq!(size, capture["tagImage"]["Size"].as_u64());
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0], metadata["containerimage.config.digest"]);
            assert_eq!(calls[1], metadata["containerimage.digest"]);
        }
    }

    #[tokio::test]
    async fn local_image_resolution_preserves_config_preference_and_deduplicates_candidates() {
        let classic = json!({
            "containerimage.config.digest": "sha256:configuration",
            "containerimage.digest": "sha256:manifest"
        });
        for mode in ["load", "tar"] {
            let mut calls = Vec::new();
            let (id, size) = resolve_image(&classic, mode, |candidate| {
                calls.push(candidate.to_owned());
                // Zero is a valid size and must not cause a fallback.
                std::future::ready(Some(0))
            })
            .await;
            assert_eq!(id, Some("sha256:configuration"));
            assert_eq!(size, Some(0));
            assert_eq!(calls, ["sha256:configuration"]);

            let mut calls = Vec::new();
            let (id, size) = resolve_image(&classic, mode, |candidate| {
                calls.push(candidate.to_owned());
                std::future::ready(None)
            })
            .await;
            assert_eq!(id, Some("sha256:configuration"));
            assert_eq!(size, None);
            assert_eq!(calls, ["sha256:configuration", "sha256:manifest"]);

            let same = json!({
                "containerimage.config.digest": "sha256:same",
                "containerimage.digest": "sha256:same"
            });
            let mut calls = 0;
            let (id, size) = resolve_image(&same, mode, |_| {
                calls += 1;
                std::future::ready(None)
            })
            .await;
            assert_eq!((id, size), (Some("sha256:same"), None));
            assert_eq!(calls, 1);

            let m4: Value =
                serde_json::from_str(include_str!("../../contracts/buildx-containerd-m4.json"))
                    .unwrap();
            let mut calls = 0;
            let (id, size) = resolve_image(&m4, mode, |_| {
                calls += 1;
                std::future::ready(Some(123))
            })
            .await;
            assert_eq!(id, m4["containerimage.digest"].as_str());
            assert_eq!(size, Some(123));
            assert_eq!(calls, 1);
        }
    }

    #[tokio::test]
    async fn remote_image_resolution_and_absent_metadata_never_inspect_local_docker() {
        for metadata in [
            json!({}),
            json!({"containerimage.digest":"sha256:manifest"}),
            json!({"containerimage.config.digest":"sha256:configuration","containerimage.digest":"sha256:manifest"}),
        ] {
            for mode in ["push", "none"] {
                let mut calls = 0;
                let resolved = resolve_image(&metadata, mode, |_| {
                    calls += 1;
                    std::future::ready(Some(123))
                })
                .await;
                assert_eq!(resolved, (image_id(&metadata, mode), None));
                assert_eq!(calls, 0);
            }
        }
        let mut calls = 0;
        let empty = json!({});
        let resolved = resolve_image(&empty, "load", |_| {
            calls += 1;
            std::future::ready(Some(123))
        })
        .await;
        assert_eq!(resolved, (None, None));
        assert_eq!(calls, 0);
    }

    #[test]
    fn captured_containerd_metadata_identifies_local_images_without_config_digest() {
        let metadata: Value =
            serde_json::from_str(include_str!("../../contracts/buildx-containerd-m4.json"))
                .unwrap();
        assert!(metadata.get("containerimage.config.digest").is_none());
        let actual_inspected_id =
            "sha256:1fcc3b8ec475d13e2c130c489c36f4f0dde2003636ca58f80eb9d3e34b6e6976";
        assert_eq!(image_id(&metadata, "load"), Some(actual_inspected_id));
        assert_eq!(image_id(&metadata, "tar"), Some(actual_inspected_id));
        assert_eq!(image_id(&metadata, "push"), None);
        assert_eq!(image_id(&metadata, "none"), None);
        assert_eq!(image_id(&json!({}), "load"), None);
        let classic = json!({
            "containerimage.config.digest": "sha256:configuration",
            "containerimage.digest": "sha256:manifest"
        });
        for mode in ["load", "tar", "push", "none"] {
            assert_eq!(image_id(&classic, mode), Some("sha256:configuration"));
        }
    }

    #[test]
    fn source_buildx_flags_map_spec_without_exposing_secrets() {
        let ctx = test_context(
            json!({"kind":"docker-build","stage":"runner","buildArgs":{"VERSION":"1.0"},"secretEnv":{"NPM_TOKEN":"secret-value"},"tags":["app:dev"],"platforms":["linux/amd64"],"noCache":true,"output":"none"}),
            PathBuf::from("/work"),
        );
        let args = build_args(
            &ctx,
            Path::new("/s"),
            Path::new("/s/Dockerfile"),
            Path::new("/m.json"),
        )
        .unwrap();
        let text = args.join(" ");
        for expected in [
            "--target runner",
            "--build-arg VERSION=1.0",
            "--secret id=NPM_TOKEN,env=NPM_TOKEN",
            "--label io.gridstator.job=J",
            "--output type=cacheonly",
            "--platform linux/amd64",
            "--no-cache",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("secret-value"));
        assert_eq!(args.last().map(String::as_str), Some("/s"));
    }
}
