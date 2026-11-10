//! Managed act archives, verified on the controller and again on the worker.
use super::*;
use anyhow::bail;
use sha2::Digest;
use tokio::io::AsyncWriteExt;
pub const PIN: &str = "0.2.97";
const MAX_TOOL: u64 = 512 * 1024 * 1024;
fn target_file(target: &str) -> Result<&'static str> {
    Ok(match target {
        "linux/amd64" => "act_Linux_x86_64.tar.gz",
        "linux/arm64" => "act_Linux_arm64.tar.gz",
        "darwin/amd64" => "act_Darwin_x86_64.tar.gz",
        "darwin/arm64" => "act_Darwin_arm64.tar.gz",
        _ => bail!("unsupported managed act target"),
    })
}
fn pin(target: &str) -> Result<&'static str> {
    Ok(match target {
        "linux/amd64" => "67b4976a56f33ea7870b798912e738ec44eef38e7578ae7b532f2def1c73a5ad",
        "linux/arm64" => "44d48e3d4a6409053fdec6fc6518322467d79a2d5be8f72c021b0421e2ac113d",
        "darwin/amd64" => "cba9cf071f69c47b2fef60a4c735002b6cbd416533920406f72eac0cf7ffec3a",
        "darwin/arm64" => "6dbb1ef34a7e34bb1ee0db188d1682ea35b82d9d9012626497f6c659e55a7cb5",
        _ => bail!("unsupported managed act target"),
    })
}
pub fn wanted(settings: &Value) -> Result<Option<(String, &'static str)>> {
    match settings["toolchain"].get("act") {
        None | Some(Value::Null) => Ok(Some((PIN.into(), "pinned"))),
        Some(Value::String(version)) if version == "off" => Ok(None),
        Some(Value::String(version)) => {
            ensure!(
                regex::Regex::new(r"^\d+\.\d+\.\d+$")?.is_match(version),
                "BAD_REQUEST: act override must be a version, pinned or off"
            );
            Ok(Some((version.clone(), "override")))
        }
        _ => bail!("BAD_REQUEST: invalid managed act setting"),
    }
}
fn key(version: &str, target: &str) -> String {
    format!("act-{version}-{}", target.replace('/', "-"))
}
fn node_target(node: &Value) -> Option<&'static str> {
    crate::release::target_for(
        node["info"]["platform"].as_str()?,
        node["info"]["arch"].as_str()?,
    )
}
async fn settings(controller: &Controller) -> Result<Value> {
    controller
        .db
        .call(|connection| {
            Ok(db::get(connection, "settings", "cluster")?.unwrap_or_else(default_settings))
        })
        .await
}
async fn entry(controller: &Controller, version: &str, target: &str) -> Result<Value> {
    let key = key(version, target);
    controller
        .db
        .call(move |connection| {
            Ok(db::get(connection, "toolchain", &key)?.unwrap_or(
                json!({"state":"preparing","reason":"waiting for the controller mirror"}),
            ))
        })
        .await
}
async fn prepare(
    controller: Arc<Controller>,
    version: String,
    target: String,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    target_file(&target)?;
    let record = key(&version, &target);
    let claim_key = record.clone();
    let now = state::now();
    let pid = std::process::id();
    let claimed = controller
        .db
        .write(move |connection| {
            let prior = db::get(connection, "toolchain", &claim_key)?.unwrap_or(Value::Null);
            if prior["state"] == "ready"
                && prior["path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).is_file())
            {
                return Ok(false);
            }
            if prior["state"] == "preparing"
                && prior["pid"] == pid
                && now - prior["at"].as_i64().unwrap_or(0) < 15 * 60 * 1000
            {
                return Ok(false);
            }
            if prior["retryAt"].as_i64().is_some_and(|retry| retry > now) {
                return Ok(false);
            }
            db::put(
                connection,
                "toolchain",
                &claim_key,
                &json!({"state":"preparing","pid":pid,"at":now,"reason":null}),
            )?;
            Ok(true)
        })
        .await?;
    if !claimed {
        return Ok(());
    }
    let result = async {
        let file = target_file(&target)?;
        let base = format!("https://github.com/nektos/act/releases/download/v{version}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .user_agent(format!("gridstator/{}", crate::VERSION))
            .build()?;
        let expected = if version == PIN {
            pin(&target)?.to_owned()
        } else {
            let mut response =
                update_network(cancel, client.get(format!("{base}/checksums.txt")).send())
                    .await?
                    .error_for_status()?;
            let mut bytes = Vec::new();
            while let Some(chunk) = update_network(cancel, response.chunk()).await? {
                ensure!(
                    bytes.len() + chunk.len() <= 1024 * 1024,
                    "act checksum document exceeds size limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            crate::release::checksum_for(std::str::from_utf8(&bytes)?, file)?
        };
        let directory = controller
            .dir
            .join("managed-tools")
            .join(&version)
            .join(target.replace('/', "-"));
        state::private_dir(&directory)?;
        let path = directory.join("act.tar.gz");
        if let Ok((size, digest)) = crate::release::hash_file(&path)
            && digest == expected
        {
            return Ok::<Value, anyhow::Error>(
                json!({"state":"ready","path":path,"sha256":expected,"size":size,"reason":null}),
            );
        }
        let mut response = update_network(cancel, client.get(format!("{base}/{file}")).send())
            .await?
            .error_for_status()?;
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_TOOL),
            "act archive exceeds size limit"
        );
        let temporary = tempfile::NamedTempFile::new_in(&directory)?;
        let mut output = tokio::fs::File::from_std(temporary.reopen()?);
        let mut hash = sha2::Sha256::new();
        let mut size = 0u64;
        while let Some(bytes) = update_network(cancel, response.chunk()).await? {
            size += bytes.len() as u64;
            ensure!(size <= MAX_TOOL, "act archive exceeds size limit");
            hash.update(&bytes);
            output.write_all(&bytes).await?;
        }
        output.sync_all().await?;
        drop(output);
        ensure!(
            hex::encode(hash.finalize()) == expected,
            "act archive SHA-256 mismatch"
        );
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(json!({"state":"ready","path":path,"sha256":expected,"size":size,"reason":null}))
    }
    .await;
    let row = match result {
        Ok(row) => row,
        Err(error) => {
            json!({"state":"failed","reason":format!("{error:#}"),"retryAt":state::now()+5*60*1000})
        }
    };
    controller
        .db
        .write(move |connection| db::put(connection, "toolchain", &record, &row))
        .await?;
    Ok(())
}
pub async fn tick(controller: Arc<Controller>) -> Result<()> {
    tick_with_cancel(controller, tokio_util::sync::CancellationToken::new()).await
}
pub async fn tick_with_cancel(
    controller: Arc<Controller>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let Some((version, _)) = wanted(&settings(&controller).await?)? else {
        return Ok(());
    };
    let nodes = controller
        .db
        .call(|connection| db::list(connection, "nodes"))
        .await?;
    let mut targets = std::collections::BTreeSet::new();
    for node in nodes {
        if (managed(&node) || update_only(&node, state::now()))
            && let Some(target) = node_target(&node).or_else(|| {
                node["updateOnly"]["target"].as_str().and_then(|target| {
                    crate::release::SUPPORTED_TARGETS
                        .into_iter()
                        .find(|candidate| *candidate == target)
                })
            })
        {
            targets.insert(target.to_owned());
        }
    }
    for target in targets {
        if cancel.is_cancelled() {
            break;
        }
        prepare(controller.clone(), version.clone(), target, &cancel).await?;
    }
    Ok(())
}
pub async fn offer(controller: &Controller, target: &str) -> Result<Value> {
    target_file(target)?;
    let Some((version, _)) = wanted(&settings(controller).await?)? else {
        return Ok(json!({"policy":"off","tools":[]}));
    };
    let entry = entry(controller, &version, target).await?;
    Ok(
        json!({"policy":"managed","tools":[{"name":"act","version":version,"platform":target,"target":target,"state":entry["state"],"sha256":entry.get("sha256").cloned().unwrap_or(Value::Null),"size":entry.get("size").cloned().unwrap_or(Value::Null),"bin":"act","path":format!("/v2/agent/toolchain/artifact?version={version}&target={target}"),"reason":entry.get("reason").cloned().unwrap_or(Value::Null)}]}),
    )
}
pub async fn expected(controller: &Controller) -> Result<Value> {
    let Some((version, _)) = wanted(&settings(controller).await?)? else {
        return Ok(json!({}));
    };
    let mut expected = json!({});
    for target in crate::release::SUPPORTED_TARGETS {
        if entry(controller, &version, target).await?["state"] == "ready" {
            expected[target] = json!(version);
        }
    }
    Ok(expected)
}
pub async fn view(controller: &Controller) -> Result<Value> {
    let settings = settings(controller).await?;
    let wanted = wanted(&settings)?;
    let nodes = controller
        .db
        .call(|connection| db::list(connection, "nodes"))
        .await?;
    let mut targets = std::collections::BTreeSet::new();
    for node in nodes {
        if managed(&node)
            && let Some(target) = node_target(&node)
        {
            targets.insert(target);
        }
    }
    let mut platforms = Vec::new();
    if let Some((version, _)) = &wanted {
        for target in targets {
            let entry = entry(controller, version, target).await?;
            platforms.push(json!({"platform":target,"state":entry["state"],"reason":entry.get("reason").cloned().unwrap_or(Value::Null)}));
        }
    }
    Ok(
        json!([{"name":"act","version":wanted.as_ref().map(|(version,_)|version),"source":wanted.as_ref().map(|(_,source)|*source).unwrap_or("off"),"platforms":platforms}]),
    )
}
pub async fn artifact(controller: &Controller, version: &str, target: &str) -> Result<PathBuf> {
    target_file(target)?;
    let Some((wanted, _)) = wanted(&settings(controller).await?)? else {
        bail!("NOT_FOUND: managed act is disabled");
    };
    ensure!(
        version == wanted,
        "NOT_FOUND: act version is no longer offered"
    );
    let entry = entry(controller, version, target).await?;
    ensure!(
        entry["state"] == "ready",
        "UNAVAILABLE: managed act archive is not ready"
    );
    let path = PathBuf::from(
        entry["path"]
            .as_str()
            .context("act archive path unavailable")?,
    );
    let expected = entry["sha256"]
        .as_str()
        .context("act archive digest unavailable")?
        .to_owned();
    let copy = path.clone();
    let (_, digest) =
        tokio::task::spawn_blocking(move || crate::release::hash_file(copy)).await??;
    ensure!(digest == expected, "act mirror archive was modified");
    Ok(path)
}

/// Safe extraction reads only the selected regular executable. It never materializes member paths.
pub fn install_archive(archive: &Path, destination: &Path, target: &str) -> Result<String> {
    let decoder = flate2::read::GzDecoder::new(fs::File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    let parent = destination
        .parent()
        .context("act destination needs a parent")?;
    state::private_dir(parent)?;
    let mut extracted = None;
    let mut total = 0u64;
    for (index, entry) in tar.entries()?.enumerate() {
        ensure!(index < 1024, "Too many managed act archive entries");
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        ensure!(
            !path.starts_with('/')
                && !path.contains('\\')
                && !path.split('/').any(|part| part == ".."),
            "Unsafe managed act archive path"
        );
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "Managed act archive links are not allowed"
        );
        total = total
            .checked_add(entry.size())
            .context("Managed act archive overflow")?;
        ensure!(
            total <= MAX_TOOL,
            "Managed act archive expansion exceeds limit"
        );
        if path.trim_start_matches("./") == "act" && kind.is_file() {
            ensure!(extracted.is_none(), "Duplicate managed act executable");
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            std::io::copy(&mut entry, &mut temporary)?;
            temporary.as_file().sync_all()?;
            crate::release::verify_executable_target(temporary.path(), target)?;
            extracted = Some(temporary);
        }
    }
    let temporary = extracted.context("Managed act archive has no regular act executable")?;
    let (_, digest) = crate::release::hash_file(temporary.path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(digest)
}
pub async fn sync_worker(check: &crate::worker::UpdateCheck) -> Result<bool> {
    if std::env::var_os("GRIDSTATOR_ACT").is_some() {
        return Ok(false);
    }
    let query = format!(
        "/v2/agent/toolchain?target={}",
        crate::release::host_target()?
    );
    let offered = update_network(&check.cancel, check.client.get(&query)).await?;
    let config_path = check.dir.join("agent.json");
    if offered["policy"] == "off" {
        let credential = check.client.config.token.clone();
        let changed = tokio::task::spawn_blocking(move || {
            let mut changed = false;
            state::update_json(&config_path, |config| {
                ensure!(
                    config["credential"] == credential,
                    "Worker credentials changed while updating managed act"
                );
                changed = config.get("managedActPath").is_some();
                if let Some(object) = config.as_object_mut() {
                    for key in ["managedActPath", "managedActVersion", "managedActSha256"] {
                        object.remove(key);
                    }
                }
                Ok(())
            })?;
            Ok::<_, anyhow::Error>(changed)
        })
        .await??;
        return Ok(changed);
    }
    let Some(tool) = offered["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "act"))
    else {
        return Ok(false);
    };
    if tool["state"] != "ready" {
        return Ok(false);
    }
    let version = tool["version"]
        .as_str()
        .context("Managed act version required")?;
    ensure!(
        regex::Regex::new(r"^\d+\.\d+\.\d+$")?.is_match(version),
        "Invalid managed act version"
    );
    let target = crate::release::host_target()?;
    let destination = check
        .dir
        .join("toolchain/act")
        .join(version)
        .join(target.replace('/', "-"))
        .join("act");
    let config = state::read_json(&config_path)?;
    if config["managedActVersion"] == version
        && config["managedActPath"] == destination.to_string_lossy().as_ref()
    {
        let copy = destination.clone();
        let expected = config["managedActSha256"].as_str().map(str::to_owned);
        if let Ok(Ok((_, digest))) =
            tokio::task::spawn_blocking(move || crate::release::hash_file(copy)).await
            && expected.as_deref() == Some(&digest)
        {
            return Ok(false);
        }
    }
    let path = tool["path"]
        .as_str()
        .context("Managed act download path required")?;
    ensure!(
        path.starts_with("/v2/agent/toolchain/artifact?"),
        "Managed act must download from the pinned controller"
    );
    let expected = tool["sha256"]
        .as_str()
        .context("Managed act digest required")?;
    let size = tool["size"].as_u64().context("Managed act size required")?;
    ensure!(size <= MAX_TOOL, "Managed act archive exceeds limit");
    let directory = check.dir.join("updates");
    state::private_dir(&directory)?;
    let temporary = tempfile::NamedTempFile::new_in(directory)?;
    let mut file = tokio::fs::File::from_std(temporary.reopen()?);
    let mut response = update_network(
        &check.cancel,
        check
            .client
            .http
            .get(format!("{}{}", check.client.config.url, path))
            .bearer_auth(&check.client.config.token)
            .send(),
    )
    .await?
    .error_for_status()?;
    let mut received = 0u64;
    let mut hash = sha2::Sha256::new();
    while let Some(bytes) = update_network(&check.cancel, response.chunk()).await? {
        received += bytes.len() as u64;
        ensure!(
            received <= size,
            "Managed act archive exceeds announced size"
        );
        hash.update(&bytes);
        file.write_all(&bytes).await?;
    }
    file.sync_all().await?;
    drop(file);
    ensure!(
        received == size && hex::encode(hash.finalize()) == expected,
        "Managed act archive size/SHA-256 mismatch"
    );
    let archive = temporary.path().to_owned();
    let installed = destination.clone();
    let target = target.to_owned();
    ensure!(
        !check.cancel.is_cancelled(),
        "Worker update cancelled before managed act installation"
    );
    let digest = tokio::task::spawn_blocking(move || {
        let _temporary = temporary;
        install_archive(&archive, &installed, &target)
    })
    .await??;
    let version = version.to_owned();
    let credential = check.client.config.token.clone();
    tokio::task::spawn_blocking(move || {
        state::update_json(&config_path, |config| {
            ensure!(
                config["credential"] == credential,
                "Worker credentials changed while updating managed act"
            );
            config["managedActPath"] = json!(destination);
            config["managedActVersion"] = json!(version);
            config["managedActSha256"] = json!(digest);
            Ok(())
        })
    })
    .await??;
    Ok(true)
}
