use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub start_at_sign_in: bool,
    pub native_cli_path: Option<String>,
    pub targets: Vec<Target>,
    pub last_startup_session: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            start_at_sign_in: true,
            native_cli_path: None,
            targets: vec![],
            last_startup_session: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub distribution: String,
    pub registration: String,
    pub linux_user: String,
    pub cli_path: String,
    pub data_dir: String,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub ca_fingerprint: String,
    pub start_at_sign_in: bool,
    pub cached: serde_json::Value,
}

pub fn path() -> Result<PathBuf> {
    if let Some(override_path) = std::env::var_os("GRIDSTATOR_DESKTOP_CONFIG") {
        let path = PathBuf::from(override_path);
        ensure!(
            path.is_absolute(),
            "GRIDSTATOR_DESKTOP_CONFIG must be an absolute path"
        );
        return Ok(path);
    }
    Ok(dirs::config_dir()
        .context("This account has no configuration directory")?
        .join("gridstator-desktop/settings.json"))
}
fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            !metadata.file_type().is_symlink(),
            "Desktop settings cannot be a symbolic link"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
pub fn load(path: &Path) -> Result<Settings> {
    reject_symlink(path)?;
    match fs::read(path) {
        Ok(bytes) => {
            ensure!(
                bytes.len() <= 1_048_576,
                "Desktop settings exceed their size limit"
            );
            Ok(serde_json::from_slice(&bytes)
                .context("Desktop settings are invalid; restore or rename the settings file")?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}
pub fn save(path: &Path, settings: &Settings) -> Result<()> {
    let directory = path
        .parent()
        .context("Desktop settings need a parent directory")?;
    reject_symlink(directory)?;
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    reject_symlink(path)?;
    let temporary = directory.join(format!(".settings-{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(settings)?)?;
        file.sync_all()?;
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let from = temporary
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let to = path
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            ensure!(
                unsafe {
                    MoveFileExW(
                        from.as_ptr(),
                        to.as_ptr(),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                } != 0,
                "Could not atomically replace desktop settings: {}",
                std::io::Error::last_os_error()
            );
        }
        #[cfg(not(windows))]
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_corrupt_settings_are_not_reset() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("desktop/settings.json");
        let settings = Settings {
            start_at_sign_in: false,
            ..Settings::default()
        };
        save(&path, &settings)?;
        assert!(!load(&path)?.start_at_sign_in);
        fs::write(&path, b"broken")?;
        assert!(load(&path).is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn settings_are_private_and_reject_symlinks() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("desktop/settings.json");
        save(&path, &Settings::default())?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let link = directory.path().join("link");
        symlink(&path, &link)?;
        assert!(load(&link).is_err());
        assert!(save(&link, &Settings::default()).is_err());
        Ok(())
    }
}
