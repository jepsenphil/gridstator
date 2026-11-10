//! OS login startup is separate from opening the companion. Neither path clears local Pause.
use anyhow::{Context, Result};

pub fn register_worker_startup() -> Result<()> {
    if cfg!(windows) || std::env::var_os("GRIDSTATOR_DESKTOP_TEST_MODE").is_some() {
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("Gridstator workers")
        .set_app_path(
            executable
                .to_str()
                .context("The desktop executable path is not UTF-8")?,
        )
        .set_args(&["--worker-startup"])
        .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
        .build()?
        .enable()?;
    Ok(())
}
pub fn session_key() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        if let Ok(session) = std::env::var("XDG_SESSION_ID")
            && !session.is_empty()
        {
            return Some(format!("linux:{}:{session}", boot.trim()));
        }
        if let Ok(session) = std::fs::read_to_string("/proc/self/sessionid")
            && session.trim() != "4294967295"
        {
            return Some(format!("linux:{}:{}", boot.trim(), session.trim()));
        }
        let display = std::env::var_os("WAYLAND_DISPLAY")
            .and_then(|display| {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(|runtime| std::path::PathBuf::from(runtime).join(display))
            })
            .or_else(|| {
                std::env::var("DISPLAY").ok().and_then(|display| {
                    display.strip_prefix(':').map(|display| {
                        std::path::PathBuf::from(format!(
                            "/tmp/.X11-unix/X{}",
                            display.split('.').next().unwrap_or(display)
                        ))
                    })
                })
            });
        if let Some(display) = display.and_then(|path| std::fs::metadata(path).ok()) {
            use std::os::unix::fs::MetadataExt;
            return Some(format!(
                "linux:{}:{}:{}",
                boot.trim(),
                display.ctime(),
                display.ctime_nsec()
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Apple AuthSession.h: callerSecuritySession is UINT32_MAX; IDs identify login sessions.
        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SessionGetInfo(session: u32, actual: *mut u32, attributes: *mut u32) -> i32;
        }
        let mut actual = 0;
        let mut attributes = 0;
        if unsafe { SessionGetInfo(u32::MAX, &mut actual, &mut attributes) } == 0 && actual != 0 {
            let boot = std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "kern.boottime"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .unwrap_or_default();
            return Some(format!("macos:{boot}:{actual}"));
        }
    }
    None
}
pub fn apply_login_startup() -> Result<()> {
    if std::env::var_os("GRIDSTATOR_DESKTOP_TEST_MODE").is_some() {
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let session =
            session_key().unwrap_or_else(|| format!("explicit-login-event-{}", std::process::id()));
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let backend = crate::backend::Backend::new(packaged_resources())?;
                backend.sign_in_startup(&session).await
            })?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn packaged_resources() -> Option<std::path::PathBuf> {
    #[cfg(feature = "gui")]
    {
        let info = tauri::PackageInfo {
            name: "Gridstator".into(),
            version: env!("CARGO_PKG_VERSION").parse().ok()?,
            authors: env!("CARGO_PKG_AUTHORS"),
            description: env!("CARGO_PKG_DESCRIPTION"),
            crate_name: env!("CARGO_PKG_NAME"),
        };
        tauri::utils::platform::resource_dir(&info, &tauri::Env::default()).ok()
    }
    #[cfg(not(feature = "gui"))]
    {
        None
    }
}
