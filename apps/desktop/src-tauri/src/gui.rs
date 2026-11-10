use crate::backend::{Action, Backend, ChangeSettings, Install, Logs, Setup, Snapshot};
use crate::{bridge, settings};
use std::{sync::Arc, time::Duration};
use tauri::{
    Emitter, Manager, State,
    menu::{Menu, MenuItem, Submenu},
    tray::TrayIconBuilder,
};
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;

type Desktop = Arc<Backend>;
struct StartupWarning(std::sync::Mutex<Option<String>>);
fn annotate(app: &tauri::AppHandle, mut snapshot: Snapshot) -> Snapshot {
    if let Some(state) = app.try_state::<StartupWarning>()
        && let Ok(warning) = state.0.lock()
        && let Some(warning) = warning.as_ref()
    {
        snapshot.error = Some(match snapshot.error.take() {
            Some(existing) => format!("{existing} {warning}"),
            None => warning.clone(),
        });
    }
    snapshot
}
fn set_companion_startup(app: &tauri::AppHandle, start: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = app;
        let executable = std::env::current_exe().map_err(safe)?;
        let manager = auto_launch::AutoLaunchBuilder::new()
            .set_app_name("Gridstator")
            .set_app_path(
                executable
                    .to_str()
                    .ok_or("The desktop executable path is not UTF-8")?,
            )
            .set_args(&["--startup"])
            .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
            .build()
            .map_err(safe)?;
        if start {
            manager.enable().map_err(safe)
        } else {
            manager.disable().map_err(safe)
        }
    }
    #[cfg(not(windows))]
    {
        if start {
            app.autolaunch().enable().map_err(safe)
        } else {
            app.autolaunch().disable().map_err(safe)
        }
    }
}
fn safe(error: impl std::fmt::Display) -> String {
    bridge::redact(&error.to_string())
}
#[cfg(not(windows))]
async fn snapshot(backend: &Backend) -> Result<Snapshot, String> {
    backend.snapshot().await.map_err(safe)
}
#[cfg(windows)]
async fn snapshot(_backend: &Backend) -> Result<Snapshot, String> {
    crate::windows_host::request(crate::windows_host::HostRequest::Snapshot)
        .await
        .map_err(safe)
        .and_then(|value| serde_json::from_value(value).map_err(safe))
}
#[tauri::command]
async fn desktop_snapshot(
    app: tauri::AppHandle,
    backend: State<'_, Desktop>,
) -> Result<Snapshot, String> {
    snapshot(backend.inner())
        .await
        .map(|snapshot| annotate(&app, snapshot))
}
#[tauri::command]
async fn desktop_action(backend: State<'_, Desktop>, request: Action) -> Result<Snapshot, String> {
    action(backend.inner(), request).await
}
async fn action(backend: &Backend, request: Action) -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        let _ = backend;
        let value = crate::windows_host::request(crate::windows_host::HostRequest::Action(request))
            .await
            .map_err(safe)?;
        serde_json::from_value(value).map_err(safe)
    }
    #[cfg(not(windows))]
    {
        backend.action(request).await.map_err(safe)
    }
}
#[tauri::command]
async fn desktop_logs(
    backend: State<'_, Desktop>,
    request: Logs,
) -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    {
        let _ = backend;
        crate::windows_host::request(crate::windows_host::HostRequest::Logs(request))
            .await
            .map_err(safe)
    }
    #[cfg(not(windows))]
    {
        backend.logs(request).await.map_err(safe)
    }
}
#[tauri::command]
async fn desktop_setup(backend: State<'_, Desktop>, request: Setup) -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        let _ = backend;
        let value = crate::windows_host::request(crate::windows_host::HostRequest::Setup(request))
            .await
            .map_err(safe)?;
        serde_json::from_value(value).map_err(safe)
    }
    #[cfg(not(windows))]
    {
        backend.setup(request).await.map_err(safe)
    }
}
#[tauri::command]
async fn desktop_install(
    backend: State<'_, Desktop>,
    request: Install,
) -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        let _ = backend;
        let value =
            crate::windows_host::request(crate::windows_host::HostRequest::Install(request))
                .await
                .map_err(safe)?;
        serde_json::from_value(value).map_err(safe)
    }
    #[cfg(not(windows))]
    {
        backend.install(request).await.map_err(safe)
    }
}
#[tauri::command]
async fn desktop_settings(
    app: tauri::AppHandle,
    backend: State<'_, Desktop>,
    request: ChangeSettings,
) -> Result<Snapshot, String> {
    if let Some(start) = request.start_at_sign_in
        && std::env::var_os("GRIDSTATOR_DESKTOP_TEST_MODE").is_none()
    {
        set_companion_startup(&app, start)?;
    }
    if request.start_at_sign_in.is_some()
        && let Some(state) = app.try_state::<StartupWarning>()
        && let Ok(mut warning) = state.0.lock()
    {
        *warning = None;
    }
    #[cfg(windows)]
    {
        let _ = backend;
        let value =
            crate::windows_host::request(crate::windows_host::HostRequest::Settings(request))
                .await
                .map_err(safe)?;
        serde_json::from_value(value).map_err(safe)
    }
    #[cfg(not(windows))]
    {
        backend.change_settings(request).await.map_err(safe)
    }
}
fn open(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
fn menu(app: &tauri::AppHandle, snapshot: Option<&Snapshot>) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    if let Some(snapshot) = snapshot {
        for worker in &snapshot.workers {
            let id = worker["id"].as_str().unwrap_or("");
            let label = worker["name"].as_str().unwrap_or("Local worker");
            let title = if let Some(distribution) = worker["distribution"].as_str() {
                format!("{label} · {distribution}")
            } else {
                label.to_owned()
            };
            let state = worker["state"].as_str().unwrap_or("Unknown");
            let count = worker["activeJobs"].as_array().map_or(0, Vec::len);
            let status = MenuItem::with_id(
                app,
                format!("state:{id}"),
                format!("{state} · {count} running"),
                false,
                None::<&str>,
            )?;
            let paused = worker["paused"] == true;
            let pause = MenuItem::with_id(
                app,
                format!("{}:{id}", if paused { "resume" } else { "pause" }),
                if paused {
                    "Resume worker"
                } else {
                    "Pause worker"
                },
                true,
                None::<&str>,
            )?;
            let running = worker["running"] == true;
            let stop = MenuItem::with_id(
                app,
                format!("{}:{id}", if running { "stop" } else { "start" }),
                if running {
                    "Stop after current jobs"
                } else {
                    "Start worker"
                },
                worker["draining"] != true,
                None::<&str>,
            )?;
            menu.append(&Submenu::with_items(
                app,
                title,
                true,
                &[&status, &pause, &stop],
            )?)?;
        }
    }
    if snapshot.is_none_or(|snapshot| snapshot.workers.is_empty()) {
        menu.append(&MenuItem::with_id(
            app,
            "setup",
            "Set up this machine…",
            true,
            None::<&str>,
        )?)?;
    }
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open worker window",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        "Quit companion",
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            if !arguments.iter().any(|argument| argument == "--startup") {
                open(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--startup"]),
        ))
        .invoke_handler(tauri::generate_handler![
            desktop_snapshot,
            desktop_action,
            desktop_logs,
            desktop_setup,
            desktop_install,
            desktop_settings
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let resources = app.path().resource_dir().ok();
            let backend = Arc::new(Backend::new(resources)?);
            let mut warning = crate::startup::register_worker_startup().err().map(|error|format!("Worker sign-in startup could not be registered. Reopen the companion to retry. {}",safe(error)));
            // Keep the window available when startup registration fails or preferences need repair.
            if std::env::var_os("GRIDSTATOR_DESKTOP_TEST_MODE").is_none()
                && let Ok(config) = settings::load(&backend.settings_path) {
                    let result=set_companion_startup(app.handle(),config.start_at_sign_in);
                    if let Err(error) = result {
                        warning = Some(format!(
                            "Could not update sign-in startup. Retry it in companion settings. {}",
                            safe(error)
                        ));
                    }
                }
            #[cfg(windows)]
            if let Err(error) = crate::windows_host::ensure_host(
                std::env::args().any(|argument| argument == "--startup"),
            ) {
                warning = Some(format!(
                    "The Windows worker host could not start. Reopen the companion to retry. {}",
                    safe(error)
                ));
            }
            app.manage(StartupWarning(std::sync::Mutex::new(warning)));
            app.manage(backend.clone());
            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;
            // A missing Linux tray implementation leaves the complete application window available.
            let tray = TrayIconBuilder::with_id("workers")
                .icon(icon)
                .tooltip("Gridstator workers")
                .menu(&menu(app.handle(), None)?)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| {
                    let event = event.id().as_ref();
                    match event {
                        "quit" => app.exit(0),
                        "open" | "setup" => open(app),
                        _ => {
                            if let Some((operation, worker_id)) = event.split_once(':')
                                && ["pause", "resume", "start", "stop"].contains(&operation) {
                                    let backend = app.state::<Desktop>().inner().clone();
                                    let app = app.clone();
                                    let request = Action {
                                        worker_id: worker_id.into(),
                                        action: operation.into(),
                                        job_id: None,
                                        confirmed: None,
                                        budget: None,
                                        start_at_sign_in: None,
                                    };
                                    tauri::async_runtime::spawn(async move {
                                        match action(&backend, request).await {
                                            Ok(snapshot) => {
                                                let _ = app.emit("workers-changed", snapshot);
                                            }
                                            Err(error) => {
                                                let _ = app.emit("worker-error", error);
                                                open(&app);
                                            }
                                        }
                                    });
                                }
                        }
                    }
                })
                .build(app)
                .ok();
            if std::env::args().any(|argument| argument == "--startup") && tray.is_some()
                && let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match snapshot(&backend).await {
                        Ok(snapshot) => {
                            let snapshot = annotate(&app, snapshot);
                            if let Some(tray) = app.tray_by_id("workers") {
                                if let Ok(menu) = menu(&app, Some(&snapshot)) {
                                    let _ = tray.set_menu(Some(menu));
                                }
                                let count = snapshot.workers.len();
                                let active = snapshot
                                    .workers
                                    .iter()
                                    .map(|worker| {
                                        worker["activeJobs"].as_array().map_or(0, Vec::len)
                                    })
                                    .sum::<usize>();
                                let _ = tray.set_tooltip(Some(format!(
                                    "Gridstator · {count} workers · {active} running jobs"
                                )));
                            }
                            let _ = app.emit("workers-changed", snapshot);
                        }
                        Err(error) => {
                            let _ = app.emit("worker-error", error);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
            Ok(())
        });
    builder
        .run(tauri::generate_context!())
        .expect("Could not open the Gridstator companion");
}
