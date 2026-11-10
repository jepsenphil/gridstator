use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

pub const MAX_RESPONSE: u64 = 4 * 1024 * 1024;
pub fn redact(text: &str) -> String {
    regex::Regex::new(r"gs[cja]_[A-Za-z0-9_-]+")
        .expect("constant token expression")
        .replace_all(text, "[redacted]")
        .chars()
        .take(2000)
        .collect()
}
pub fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.current_dir(dirs::home_dir().unwrap_or_else(std::env::temp_dir));
    command
        .env_remove("GRIDSTATOR_URL")
        .env_remove("GRIDSTATOR_TOKEN");
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
    }
    command
}
async fn read_bounded(reader: impl AsyncRead + Unpin, maximum: u64) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    reader.take(maximum + 1).read_to_end(&mut result).await?;
    ensure!(
        result.len() as u64 <= maximum,
        "Native helper response exceeded its size limit"
    );
    Ok(result)
}
pub async fn output(mut command: Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    ensure!(
        input.len() <= MAX_RESPONSE as usize,
        "Native request exceeded its size limit"
    );
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .context("Could not start the native helper")?;
    let mut stdin = child
        .stdin
        .take()
        .context("Native helper input unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("Native helper output unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("Native helper errors unavailable")?;
    let operation = async {
        let write = async move {
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<(), anyhow::Error>(())
        };
        let (_, stdout, stderr) = tokio::try_join!(
            write,
            read_bounded(stdout, MAX_RESPONSE),
            read_bounded(stderr, 32 * 1024)
        )?;
        let status = child.wait().await?;
        ensure!(
            status.success(),
            "{}",
            redact(String::from_utf8_lossy(&stderr).trim())
        );
        Ok(stdout)
    };
    tokio::time::timeout(timeout, operation)
        .await
        .context("Native operation timed out; refresh status before retrying")?
}
pub async fn execute(path: &Path, request: &Value, timeout: Duration) -> Result<Value> {
    let mut command = command(path);
    command.arg("bridge");
    let result = output(command, &serde_json::to_vec(request)?, timeout).await?;
    serde_json::from_slice(&result).context("Native helper returned an invalid response")
}
pub fn local_cli(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        ensure!(path.is_absolute(), "Choose an absolute native CLI path");
        ensure!(
            path.is_file(),
            "The configured native CLI is missing; update its path in settings"
        );
        return std::fs::canonicalize(path).context("Cannot resolve the configured native CLI");
    }
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/gridstator"));
    }
    candidates.extend(
        [
            "/usr/local/bin/gridstator",
            "/opt/homebrew/bin/gridstator",
            "/usr/bin/gridstator",
        ]
        .map(PathBuf::from),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .context("Install the native CLI to set up or control this machine’s worker")
}
pub async fn verify(path: &Path) -> Result<()> {
    type Stamp = (u64, std::time::SystemTime, Option<std::time::SystemTime>);
    static VERIFIED: std::sync::OnceLock<
        tokio::sync::Mutex<std::collections::HashMap<PathBuf, Stamp>>,
    > = std::sync::OnceLock::new();
    fn stamp(path: &Path) -> Result<Stamp> {
        let metadata = std::fs::metadata(path)?;
        Ok((
            metadata.len(),
            metadata.modified()?,
            metadata.created().ok(),
        ))
    }
    let canonical = std::fs::canonicalize(path)?;
    let before = stamp(&canonical)?;
    let mut verified = VERIFIED.get_or_init(Default::default).lock().await;
    if verified.get(&canonical) == Some(&before) {
        return Ok(());
    }
    let mut command = command(&canonical);
    command.arg("build-info").arg("--json");
    let bytes = output(command, &[], Duration::from_secs(10)).await?;
    validate_build_info(&serde_json::from_slice(&bytes)?)?;
    ensure!(
        before == stamp(&canonical)?,
        "The native CLI changed during verification; refresh to retry"
    );
    verified.insert(canonical, before);
    Ok(())
}
pub fn validate_build_info(info: &Value) -> Result<()> {
    ensure!(
        info["version"]
            .as_str()
            .is_some_and(|version| version.starts_with("2.")),
        "This companion requires the native Gridstator 2 CLI"
    );
    for capability in ["bridge-v1", "worker-control-v1", "local-setup-v1"] {
        ensure!(
            info["capabilities"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == capability)),
            "The native CLI lacks {capability}; install a matching native release"
        );
    }
    Ok(())
}
pub fn worker_request(action: &str) -> Value {
    json!({"version":1,"operation":"worker","action":action})
}

#[derive(Debug)]
pub struct JoinCredentials {
    pub url: String,
    pub fingerprint: String,
    pub token: String,
}
pub fn join_credentials(
    connection: Option<&str>,
    url: Option<&str>,
    fingerprint: Option<&str>,
    token: Option<&str>,
) -> Result<JoinCredentials> {
    let invalid = || {
        anyhow::anyhow!(
            "Paste a worker join command, or enter its HTTPS URL, SHA-256 fingerprint and worker join token"
        )
    };
    let (endpoint, pin, secret) =
        if let Some(input) = connection.filter(|input| !input.trim().is_empty()) {
            ensure!(input.len() <= 16_384, "Invalid worker join command");
            let input = input.replace("\r\n", "\n");
            ensure!(
                !input.contains(['\0', '\r', ';', '&', '|', '>', '<', '`', '$']),
                "Invalid worker join command"
            );
            let parts = shell_words::split(&input).map_err(|_| invalid())?;
            if parts.len() == 3 && parts[0].starts_with("https://") {
                (parts[0].clone(), parts[1].clone(), parts[2].clone())
            } else {
                ensure!(
                    parts.len() >= 4
                        && Path::new(&parts[0])
                            .file_name()
                            .is_some_and(|name| name == "gridstator" || name == "gridstator.exe")
                        && parts[1] == "agent"
                        && parts[2] == "join",
                    "Expected a Gridstator worker join command"
                );
                let mut options = std::collections::HashMap::new();
                let mut index = 3;
                while index < parts.len() {
                    let option = &parts[index];
                    if !option.starts_with("--") {
                        ensure!(
                            options
                                .insert("--url".to_owned(), option.to_owned())
                                .is_none(),
                            "Only one controller URL is allowed"
                        );
                        index += 1;
                        continue;
                    }
                    let (key, value) = match option.split_once('=') {
                        Some((key, value)) => (key, value),
                        None => {
                            index += 1;
                            (
                                option.as_str(),
                                parts.get(index).map(String::as_str).ok_or_else(invalid)?,
                            )
                        }
                    };
                    ensure!(
                        ["--url", "--fingerprint", "--token", "--name"].contains(&key)
                            && !value.starts_with("--")
                            && options.insert(key.to_owned(), value.to_owned()).is_none(),
                        "Invalid worker join options"
                    );
                    index += 1;
                }
                (
                    options.remove("--url").ok_or_else(invalid)?,
                    options.remove("--fingerprint").ok_or_else(invalid)?,
                    options.remove("--token").ok_or_else(invalid)?,
                )
            }
        } else {
            (
                url.ok_or_else(invalid)?.into(),
                fingerprint.ok_or_else(invalid)?.into(),
                token.ok_or_else(invalid)?.into(),
            )
        };
    let parsed = url::Url::parse(endpoint.trim()).map_err(|_| invalid())?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "Enter an HTTPS controller URL without credentials, a path or a query"
    );
    let pin = pin
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(pin.trim())
        .replace(':', "");
    ensure!(
        pin.len() == 64 && pin.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Enter a SHA-256 fingerprint with 64 hexadecimal digits"
    );
    ensure!(
        secret.starts_with("gsj_")
            && secret.len() <= 4096
            && secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "Use a worker join token; client login tokens cannot register workers"
    );
    Ok(JoinCredentials {
        url: parsed.as_str().trim_end_matches('/').into(),
        fingerprint: pin.to_ascii_lowercase(),
        token: secret,
    })
}
pub async fn install_release_tag(target: &str) -> Result<String> {
    let metadata: Value = serde_json::from_str(include_str!("../resources/native-install.json"))?;
    if let Some(tag) = metadata["tag"].as_str() {
        return validate_tag(tag);
    }
    if let Some(tag) = option_env!("GRIDSTATOR_NATIVE_INSTALL_TAG").filter(|tag| !tag.is_empty()) {
        return validate_tag(tag);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client
        .get("https://api.github.com/repos/jepsenphil/gridstator/releases?per_page=100")
        .header("User-Agent", "gridstator-desktop")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE as usize,
            "Release metadata exceeded its size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let releases: Vec<Value> = serde_json::from_slice(&bytes)?;
    for release in releases {
        let Some(tag) = release["tag_name"].as_str() else {
            continue;
        };
        if release["draft"] != false || validate_tag(tag).is_err() {
            continue;
        }
        let archive = format!("gridstator-{}-{target}.tar.gz", &tag[1..]);
        if ["gridstator-release.json", "SHA256SUMS", archive.as_str()]
            .iter()
            .all(|name| {
                release["assets"]
                    .as_array()
                    .is_some_and(|assets| assets.iter().any(|asset| asset["name"] == *name))
            })
        {
            return Ok(tag.into());
        }
    }
    bail!(
        "No matching native release is published yet. Choose a trusted native CLI path in settings or retry after a compatible release is published"
    )
}
pub struct BundledRelease {
    pub directory: PathBuf,
    pub tag: String,
    pub manifest_sha256: String,
    pub archive_sha256: String,
}
pub fn bundled_release(resources: Option<&Path>, target: &str) -> Result<Option<BundledRelease>> {
    let metadata: Value = serde_json::from_str(include_str!("../resources/native-install.json"))?;
    bundled_release_from(&metadata, resources, target)
}
fn bundled_release_from(
    metadata: &Value,
    resources: Option<&Path>,
    target: &str,
) -> Result<Option<BundledRelease>> {
    let Some(bundle) = metadata.get("bundle") else {
        return Ok(None);
    };
    ensure!(
        bundle["targets"]
            .as_array()
            .is_some_and(|targets| targets.iter().any(|entry| entry == target)),
        "This installer does not include a native CLI for {target}; install a matching desktop package"
    );
    let tag = validate_tag(
        bundle["tag"]
            .as_str()
            .context("Bundled native version is missing")?,
    )?;
    let manifest_sha256 = bundle["manifestSha256"]
        .as_str()
        .context("Bundled native manifest checksum is missing")?;
    let archive_sha256 = bundle["archiveSha256"][target]
        .as_str()
        .context("Bundled native archive checksum is missing")?;
    ensure!(
        archive_sha256.len() == 64
            && archive_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid bundled native archive checksum"
    );
    ensure!(
        manifest_sha256.len() == 64
            && manifest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid bundled native manifest checksum"
    );
    let resources = resources.context(
        "The bundled native CLI resources are unavailable; reinstall the desktop package",
    )?;
    let directory = [
        resources.join("resources/native-bundle"),
        resources.join("native-bundle"),
    ]
    .into_iter()
    .find(|path| path.join("gridstator-release.json").is_file())
    .context("The bundled native CLI is missing; reinstall the desktop package")?;
    Ok(Some(BundledRelease {
        directory,
        tag,
        manifest_sha256: manifest_sha256.into(),
        archive_sha256: archive_sha256.into(),
    }))
}
fn validate_tag(tag: &str) -> Result<String> {
    ensure!(
        regex::Regex::new(r"^v2\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$")?
            .is_match(tag),
        "Invalid native release tag"
    );
    Ok(tag.into())
}
pub async fn host_session(path: &Path) -> Result<Child> {
    let mut command = command(path);
    command
        .arg("bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .context("Host input unavailable")?
        .write_all(b"{\"version\":1,\"operation\":\"workerHost\"}")
        .await?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_bundle_selection_requires_the_packaged_target_and_resources() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let resources = directory.path().join("application path with spaces");
        let bundle = resources.join("resources/native-bundle");
        std::fs::create_dir_all(&bundle)?;
        let manifest = bundle.join("gridstator-release.json");
        std::fs::write(&manifest, b"manifest integrity is checked by the installer")?;
        let metadata = json!({"bundle":{"tag":"v2.0.0-dev.1","targets":["linux/amd64"],"manifestSha256":"ab".repeat(32),"archiveSha256":{"linux/amd64":"cd".repeat(32)}}});
        let selected = bundled_release_from(&metadata, Some(&resources), "linux/amd64")?
            .context("bundle must be selected before release discovery")?;
        assert_eq!(selected.directory, bundle);
        assert_eq!(selected.manifest_sha256, "ab".repeat(32));
        assert!(bundled_release_from(&metadata, Some(&resources), "linux/arm64").is_err());
        assert!(bundled_release_from(&metadata, None, "linux/amd64").is_err());
        std::fs::remove_file(manifest)?;
        assert!(bundled_release_from(&metadata, Some(&resources), "linux/amd64").is_err());
        assert!(bundled_release_from(&json!({}), None, "linux/amd64")?.is_none());
        Ok(())
    }
    #[test]
    fn join_parser_never_accepts_client_tokens_or_shell_commands() {
        let pin = "ab".repeat(32);
        let paste = format!(
            "gridstator agent join --url 'https://controller:7300' --fingerprint {pin} --token gsj_abc"
        );
        assert_eq!(
            join_credentials(Some(&paste), None, None, None)
                .unwrap()
                .token,
            "gsj_abc"
        );
        let printed = format!(
            "gridstator agent join https://controller:7300 --fingerprint {pin} --token gsj_abc"
        );
        assert_eq!(
            join_credentials(Some(&printed), None, None, None)
                .unwrap()
                .url,
            "https://controller:7300"
        );
        let reordered = format!(
            "gridstator agent join --token 'gsj_abc'\r\n --fingerprint {pin} 'https://controller:7300'"
        );
        assert!(join_credentials(Some(&reordered), None, None, None).is_ok());
        for bad in [
            paste.replace("gsj_abc", "gsc_abc"),
            format!("{paste}; touch /tmp/bad"),
            paste.replace("agent join", "login"),
            paste.replace("--token gsj_abc", "--token $(secret)"),
        ] {
            assert!(join_credentials(Some(&bad), None, None, None).is_err());
        }
        assert!(!redact("failed gsj_secret gsc_other").contains("secret"));
    }
    #[test]
    fn rejects_old_helpers_and_relative_overrides() {
        assert!(local_cli(Some("./gridstator")).is_err());
        assert!(
            validate_build_info(&json!({"version":"2.0.0","capabilities":["bridge-v1"]})).is_err()
        );
        assert!(validate_build_info(&json!({"version":"2.0.0","capabilities":["bridge-v1","worker-control-v1","local-setup-v1"]})).is_ok());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn private_input_and_error_redaction() -> Result<()> {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "cat >/dev/null; printf 'failed gsj_topsecret' >&2; exit 1",
        ]);
        let error = output(command, b"private", Duration::from_secs(3))
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("topsecret"));
        assert!(
            error.to_string().contains("failed [redacted]"),
            "The subprocess must receive EOF before reading its response: {error}"
        );
        Ok(())
    }
}
