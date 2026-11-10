//! WSL arguments are always an argv vector. Enumeration never launches a distribution.
use crate::{bridge, settings::Target};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use tokio::process::{Child, Command};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Distribution {
    pub name: String,
    pub version: u32,
    pub state: String,
    pub registration: String,
}
pub fn decode_output(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe])
        || bytes
            .iter()
            .skip(1)
            .step_by(2)
            .take(32)
            .any(|byte| *byte == 0)
    {
        let offset = usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2;
        String::from_utf16_lossy(
            &bytes[offset..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
pub fn wsl_executable() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/wsl.exe")
    }
    #[cfg(not(windows))]
    PathBuf::from("wsl.exe")
}
pub fn arguments(
    distribution: &str,
    user: Option<&str>,
    executable: &str,
    args: &[&str],
) -> Result<Vec<String>> {
    ensure!(
        !distribution.is_empty()
            && distribution.len() <= 512
            && !distribution.chars().any(char::is_control),
        "Choose a valid WSL distribution"
    );
    ensure!(
        executable.starts_with('/') && !executable.chars().any(char::is_control),
        "The WSL helper path must be absolute"
    );
    let mut result = vec!["--distribution".into(), distribution.into()];
    if let Some(user) = user {
        ensure!(
            !user.is_empty()
                && user.len() <= 256
                && !user
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace()),
            "Choose a valid Linux account"
        );
        result.extend(["--user".into(), user.into()]);
    }
    result.extend(["--exec".into(), executable.into()]);
    result.extend(args.iter().map(|arg| (*arg).to_owned()));
    Ok(result)
}
pub fn command(
    distribution: &str,
    user: Option<&str>,
    executable: &str,
    args: &[&str],
) -> Result<Command> {
    let mut command = bridge::command(&wsl_executable());
    command.args(arguments(distribution, user, executable, args)?);
    Ok(command)
}
#[cfg(windows)]
pub async fn distributions() -> Result<Vec<Distribution>> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let mut running_command = bridge::command(&wsl_executable());
    running_command.args(["--list", "--running", "--quiet"]);
    let running =
        decode_output(&bridge::output(running_command, &[], Duration::from_secs(15)).await?);
    let running: std::collections::HashSet<_> = running
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let root = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss")
    {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut entries = vec![];
    for registration in root.enum_keys() {
        let registration = registration?;
        let key = root.open_subkey(&registration)?;
        let Ok(name) = key.get_value::<String, _>("DistributionName") else {
            continue;
        };
        let version = key.get_value::<u32, _>("Version").unwrap_or(1);
        let state = if running.contains(name.as_str()) {
            "running"
        } else {
            "stopped"
        };
        entries.push(Distribution {
            name,
            version,
            state: state.into(),
            registration,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}
#[cfg(not(windows))]
pub async fn distributions() -> Result<Vec<Distribution>> {
    Ok(vec![])
}

pub async fn resolve_account(
    distribution: &str,
    requested: Option<&str>,
) -> Result<(String, String)> {
    let user = bridge::output(
        command(distribution, requested, "/usr/bin/id", &["-un"])?,
        &[],
        Duration::from_secs(30),
    )
    .await?;
    let user = String::from_utf8(user)?.trim().to_owned();
    let home = bridge::output(
        command(distribution, Some(&user), "/usr/bin/printenv", &["HOME"])?,
        &[],
        Duration::from_secs(30),
    )
    .await?;
    let home = String::from_utf8(home)?.trim().to_owned();
    ensure!(
        home.starts_with('/') && !home.chars().any(char::is_control),
        "The selected Linux account has no valid home directory"
    );
    Ok((user, home))
}
pub async fn execute(
    target: &Target,
    request: &serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let result = bridge::output(
        command(
            &target.distribution,
            Some(&target.linux_user),
            &target.cli_path,
            &["bridge"],
        )?,
        &serde_json::to_vec(request)?,
        timeout,
    )
    .await?;
    serde_json::from_slice(&result).context("The WSL helper returned an invalid response")
}
pub async fn verify(target: &Target) -> Result<()> {
    let result = bridge::output(
        command(
            &target.distribution,
            Some(&target.linux_user),
            &target.cli_path,
            &["build-info", "--json"],
        )?,
        &[],
        Duration::from_secs(20),
    )
    .await?;
    bridge::validate_build_info(&serde_json::from_slice(&result)?)
}
pub async fn host_session(target: &Target) -> Result<Child> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let mut command = command(
        &target.distribution,
        Some(&target.linux_user),
        &target.cli_path,
        &["bridge"],
    )?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .context("WSL host input unavailable")?
        .write_all(&serde_json::to_vec(
            &serde_json::json!({"version":1,"operation":"workerHost","dataDir":target.data_dir}),
        )?)
        .await?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_utf16_non_ascii_and_distro_spaces() {
        let original = "  Ubuntu Work\r\n  開発環境\r\n";
        let bytes: Vec<_> = original.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_output(&bytes), original);
        let args = arguments(
            "Ubuntu Work",
            Some("pj"),
            "/home/pj/.local/bin/gridstator",
            &["bridge"],
        )
        .unwrap();
        assert_eq!(
            args,
            vec![
                "--distribution",
                "Ubuntu Work",
                "--user",
                "pj",
                "--exec",
                "/home/pj/.local/bin/gridstator",
                "bridge"
            ]
        );
        assert!(!args.iter().any(|argument| {
            ["--shutdown", "--terminate", "--unregister", "sh", "-c"].contains(&argument.as_str())
        }));
    }
    #[test]
    fn rejects_control_characters_and_relative_helpers() {
        assert!(arguments("Ubuntu\nOther", None, "/bin/helper", &[]).is_err());
        assert!(arguments("Ubuntu", Some("name with spaces"), "/bin/helper", &[]).is_err());
        assert!(arguments("Ubuntu", None, "./gridstator", &[]).is_err());
    }
}
