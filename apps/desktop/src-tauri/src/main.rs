// Preview/debug builds are desktop applications too; launching them must not open a console.
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    if std::env::args().any(|argument| argument == "--build-info" || argument == "build-info") {
        let os = if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        };
        let arch = if cfg!(target_arch = "x86_64") {
            "amd64"
        } else {
            std::env::consts::ARCH
        };
        println!(
            "{}",
            serde_json::json!({
                "version":env!("CARGO_PKG_VERSION"),
                "buildId":option_env!("GRIDSTATOR_DESKTOP_BUILD_ID").unwrap_or(env!("CARGO_PKG_VERSION")),
                "sourceRevision":option_env!("GRIDSTATOR_SOURCE_REVISION").unwrap_or("development"),
                "target":format!("{os}/{arch}"),
                "capabilities":if cfg!(windows){vec!["desktop-v1","worker-control-v1","wsl-host-v1"]}else{vec!["desktop-v1","worker-control-v1"]}
            })
        );
        return;
    }
    #[cfg(windows)]
    if std::env::args().any(|argument| argument == "--worker-host") {
        if let Err(error) = gridstator_desktop::windows_host::run() {
            eprintln!(
                "Gridstator host: {}",
                gridstator_desktop::bridge::redact(&error.to_string())
            );
        }
        return;
    }
    #[cfg(not(windows))]
    if std::env::args().any(|argument| argument == "--startup" || argument == "--worker-startup") {
        if let Err(error) = gridstator_desktop::startup::apply_login_startup() {
            eprintln!(
                "Gridstator sign-in: {}",
                gridstator_desktop::bridge::redact(&error.to_string())
            );
        }
        if std::env::args().any(|argument| argument == "--worker-startup") {
            return;
        }
    }
    gridstator_desktop::gui::run();
}
