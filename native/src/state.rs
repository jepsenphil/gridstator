//! Isolated v2 paths and private, durable file writes.
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
pub fn id() -> String {
    ulid::Ulid::new().to_string()
}
pub fn data_dir(role: &str) -> PathBuf {
    let key = if role == "controller" {
        "GRIDSTATOR_CONTROLLER_DATA"
    } else {
        "GRIDSTATOR_AGENT_DATA"
    };
    std::env::var_os(key).map(PathBuf::from).unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gridstator-v2")
            .join(role)
    })
}
pub fn config_path() -> PathBuf {
    std::env::var_os("GRIDSTATOR_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("gridstator-v2/client.json")
        })
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// A configured file may live in an existing shared/project directory. Only newly
// created parents belong to this write; changing an existing parent's mode would
// alter the caller's directory permissions. Dedicated state dirs use private_dir.
fn ensure_write_parent(parent: &Path) -> Result<&Path> {
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    Ok(parent)
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = ensure_write_parent(path.parent().context("file needs parent directory")?)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        id()
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}
/// Serialize shared state mutations. Async callers should invoke this on a blocking worker.
pub fn update_json(path: &Path, update: impl FnOnce(&mut Value) -> Result<()>) -> Result<Value> {
    let parent = ensure_write_parent(path.parent().context("state file needs a parent")?)?;
    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .context("state filename missing")?
            .to_string_lossy()
    ));
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let mut value = if path.exists() {
        read_json(path)?
    } else {
        serde_json::json!({})
    };
    update(&mut value)?;
    write_json(path, &value)?;
    Ok(value)
}
pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn atomic_config_writes_preserve_existing_parent_permissions() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let parent = temporary.path().join("shared project");
        fs::create_dir(&parent)?;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o775))?;
        let file = parent.join("client.json");
        write_json(&file, &serde_json::json!({"revision": 1}))?;
        assert_eq!(mode(&parent), 0o775);
        assert_eq!(mode(&file), 0o600);
        update_json(&file, |value| {
            value["revision"] = serde_json::json!(2);
            Ok(())
        })?;
        assert_eq!(mode(&parent), 0o775);
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&parent.join(".client.json.lock")), 0o600);
        assert_eq!(read_json(&file)?["revision"], 2);
        Ok(())
    }

    #[test]
    fn atomic_config_writes_create_private_missing_parents() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let first = temporary.path().join("new");
        let parent = first.join("nested");
        let file = parent.join("client.json");
        write_json(&file, &serde_json::json!({"private": true}))?;
        assert_eq!(mode(&first), 0o700);
        assert_eq!(mode(&parent), 0o700);
        assert_eq!(mode(&file), 0o600);
        Ok(())
    }
}
