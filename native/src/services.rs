//! Explicit service definitions and controls. Rendering/planning never executes a service command;
//! only the caller's explicit `install`, `uninstall`, or `control` operation touches the OS manager.

use crate::release::invalid;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceRole {
    Controller,
    Agent,
}
impl ServiceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Controller => "controller",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceScope {
    User,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePlatform {
    Systemd,
    Launchd,
}
impl ServicePlatform {
    pub fn current() -> io::Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Self::Systemd),
            "macos" => Ok(Self::Launchd),
            _ => Err(invalid(
                "automatic services require Linux/systemd or macOS/launchd",
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub role: ServiceRole,
    pub scope: ServiceScope,
    /// Basename without .service/.plist, e.g. gridstator-native-controller. Callers deliberately
    /// choose a separate name for development.
    pub name: String,
    pub label: String,
    pub supervisor: PathBuf,
    pub install_root: PathBuf,
    pub data_dir: PathBuf,
    pub home: PathBuf,
    pub uid: u32,
    pub user: Option<String>,
    pub group: Option<String>,
    pub supplementary_groups: Vec<String>,
    pub path_dirs: Vec<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub extra_args: Vec<String>,
    pub log_dir: PathBuf,
    pub stop_timeout_seconds: u32,
}

impl ServiceConfig {
    pub fn new(
        role: ServiceRole,
        supervisor: PathBuf,
        install_root: PathBuf,
        data_dir: PathBuf,
        home: PathBuf,
        uid: u32,
    ) -> Self {
        let name = format!("gridstator-native-{}", role.as_str());
        let mut path_dirs = vec![
            home.join(".local/bin"),
            home.join(".docker/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ];
        if let Some(parent) = supervisor.parent() {
            path_dirs.insert(0, parent.to_owned());
        }
        let log_dir = data_dir.join("service-logs");
        Self {
            role,
            scope: ServiceScope::User,
            label: format!("org.gridstator.native.{}", role.as_str()),
            name,
            supervisor,
            install_root,
            data_dir,
            home,
            uid,
            user: None,
            group: None,
            supplementary_groups: Vec::new(),
            path_dirs,
            environment: BTreeMap::new(),
            extra_args: Vec::new(),
            log_dir,
            stop_timeout_seconds: 90,
        }
    }

    pub fn unit_name(&self) -> String {
        format!("{}.service", self.name)
    }

    /// Public CLI contract for the stable native supervisor invocation. The payload's role command
    /// remains unchanged and executes from its managed version directory.
    pub fn program_arguments(&self) -> io::Result<Vec<String>> {
        self.validate()?;
        let mut args = vec![
            path_string(&self.supervisor)?,
            "supervise".into(),
            "--install-root".into(),
            path_string(&self.install_root)?,
            "--".into(),
            self.role.as_str().into(),
            "start".into(),
            "--data-dir".into(),
            path_string(&self.data_dir)?,
        ];
        args.extend(self.extra_args.clone());
        Ok(args)
    }

    pub fn environment(&self) -> io::Result<BTreeMap<String, String>> {
        let mut environment = self.environment.clone();
        // HOME is the service account's home, never the sudo caller's inherited HOME.
        environment.insert("HOME".into(), path_string(&self.home)?);
        environment
            .entry("GRIDSTATOR_LOG_FORMAT".into())
            .or_insert_with(|| "json".into());
        environment
            .entry("DOCKER_CONFIG".into())
            .or_insert(path_string(&self.home.join(".docker"))?);
        let mut dirs = Vec::new();
        for path in &self.path_dirs {
            let value = path_string(path)?;
            if value.contains(':') {
                return Err(invalid("PATH directory cannot contain a colon"));
            }
            if !dirs.contains(&value) {
                dirs.push(value);
            }
        }
        environment.insert("PATH".into(), dirs.join(":"));
        for (name, value) in &environment {
            if name.is_empty()
                || !name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
            {
                return Err(invalid("invalid service environment variable name"));
            }
            validate_text(value)?;
        }
        Ok(environment)
    }

    pub fn definition_path(&self, platform: ServicePlatform) -> io::Result<PathBuf> {
        self.validate()?;
        Ok(match (platform, self.scope) {
            (ServicePlatform::Systemd, ServiceScope::User) => self
                .home
                .join(".config/systemd/user")
                .join(self.unit_name()),
            (ServicePlatform::Systemd, ServiceScope::System) => {
                PathBuf::from("/etc/systemd/system").join(self.unit_name())
            }
            (ServicePlatform::Launchd, ServiceScope::User) => self
                .home
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", self.label)),
            (ServicePlatform::Launchd, ServiceScope::System) => {
                PathBuf::from("/Library/LaunchDaemons").join(format!("{}.plist", self.label))
            }
        })
    }

    fn validate(&self) -> io::Result<()> {
        if !safe_name(&self.name) || !safe_name(&self.label) {
            return Err(invalid("invalid service name or launchd label"));
        }
        for path in [
            &self.supervisor,
            &self.install_root,
            &self.data_dir,
            &self.home,
            &self.log_dir,
        ] {
            if !path.is_absolute() {
                return Err(invalid("service paths must be absolute"));
            }
            path_string(path)?;
        }
        for path in &self.path_dirs {
            if !path.is_absolute() {
                return Err(invalid("service PATH entries must be absolute"));
            }
            path_string(path)?;
        }
        if self.scope == ServiceScope::System && self.user.is_none() {
            return Err(invalid("system service must explicitly name its account"));
        }
        for name in self
            .user
            .iter()
            .chain(self.group.iter())
            .chain(self.supplementary_groups.iter())
        {
            if !safe_name(name) {
                return Err(invalid("invalid service user/group"));
            }
        }
        if self.stop_timeout_seconds == 0 {
            return Err(invalid("service stop timeout must be positive"));
        }
        for arg in &self.extra_args {
            validate_text(arg)?;
        }
        Ok(())
    }
}

pub fn render_systemd(config: &ServiceConfig) -> io::Result<String> {
    let args = config.program_arguments()?;
    let environment = config.environment()?;
    let mut lines = vec![
        "[Unit]".to_owned(),
        format!("Description=Gridstator native {}", config.role.as_str()),
        "After=network-online.target docker.service".into(),
        "Wants=network-online.target".into(),
        String::new(),
        "[Service]".into(),
        "Type=simple".into(),
    ];
    if config.scope == ServiceScope::System {
        lines.push(format!(
            "User={}",
            config.user.as_deref().expect("validated user")
        ));
        if let Some(group) = &config.group {
            lines.push(format!("Group={group}"));
        }
        if !config.supplementary_groups.is_empty() {
            lines.push(format!(
                "SupplementaryGroups={}",
                config.supplementary_groups.join(" ")
            ));
        }
    }
    lines.push(format!(
        "WorkingDirectory={}",
        systemd_directory(&path_string(&config.home)?)?
    ));
    lines.push(format!(
        "ExecStart={}",
        args.iter()
            .map(|arg| systemd_quote(arg, true))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    for (name, value) in environment {
        lines.push(format!(
            "Environment={}",
            systemd_quote(&format!("{name}={value}"), false)
        ));
    }
    lines.extend([
        "Restart=on-failure".into(),
        "RestartSec=5".into(),
        "SuccessExitStatus=130 143".into(),
        "KillMode=mixed".into(),
        format!("TimeoutStopSec={}", config.stop_timeout_seconds),
        String::new(),
        "[Install]".into(),
        format!(
            "WantedBy={}",
            if config.scope == ServiceScope::System {
                "multi-user.target"
            } else {
                "default.target"
            }
        ),
        String::new(),
    ]);
    Ok(lines.join("\n"))
}

pub fn render_launchd(config: &ServiceConfig) -> io::Result<String> {
    let args = config.program_arguments()?;
    let environment = config.environment()?;
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n",
    );
    plist_string(&mut out, "Label", &config.label);
    out.push_str("<key>ProgramArguments</key><array>\n");
    for arg in args {
        out.push_str(&format!("<string>{}</string>\n", xml(&arg)));
    }
    out.push_str("</array>\n<key>EnvironmentVariables</key><dict>\n");
    for (name, value) in environment {
        plist_string(&mut out, &name, &value);
    }
    out.push_str("</dict>\n");
    plist_string(&mut out, "WorkingDirectory", &path_string(&config.home)?);
    if config.scope == ServiceScope::System {
        plist_string(
            &mut out,
            "UserName",
            config.user.as_deref().expect("validated user"),
        );
        if let Some(group) = &config.group {
            plist_string(&mut out, "GroupName", group);
        }
    }
    plist_string(
        &mut out,
        "StandardOutPath",
        &path_string(&config.log_dir.join("stdout.log"))?,
    );
    plist_string(
        &mut out,
        "StandardErrorPath",
        &path_string(&config.log_dir.join("stderr.log"))?,
    );
    out.push_str("<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>5</integer>\n");
    out.push_str(&format!(
        "<key>ExitTimeOut</key><integer>{}</integer>\n",
        config.stop_timeout_seconds
    ));
    plist_string(&mut out, "ProcessType", "Background");
    out.push_str("</dict></plist>\n");
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceCommand {
    pub program: String,
    pub args: Vec<OsString>,
}
impl ServiceCommand {
    fn new(program: &str, args: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            program: program.to_owned(),
            args: args.into_iter().collect(),
        }
    }
    pub fn execute(&self) -> io::Result<Output> {
        Command::new(&self.program).args(&self.args).output()
    }
    fn checked(&self) -> io::Result<Output> {
        let output = self.execute()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{} failed ({}): {}",
                self.program,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Clone, Debug)]
pub struct ServiceManager {
    platform: ServicePlatform,
}
impl ServiceManager {
    pub fn new(platform: ServicePlatform) -> Self {
        Self { platform }
    }
    pub fn current() -> io::Result<Self> {
        Ok(Self::new(ServicePlatform::current()?))
    }

    pub fn render(&self, config: &ServiceConfig) -> io::Result<String> {
        match self.platform {
            ServicePlatform::Systemd => render_systemd(config),
            ServicePlatform::Launchd => render_launchd(config),
        }
    }

    pub fn control_commands(
        &self,
        config: &ServiceConfig,
        action: ServiceAction,
    ) -> io::Result<Vec<ServiceCommand>> {
        config.validate()?;
        Ok(match self.platform {
            ServicePlatform::Systemd => {
                let verb = match action {
                    ServiceAction::Start => "start",
                    ServiceAction::Stop => "stop",
                    ServiceAction::Restart => "restart",
                    ServiceAction::Status => "status",
                };
                vec![systemctl(config, &[verb, &config.unit_name()])]
            }
            ServicePlatform::Launchd => {
                let target = launchd_target(config);
                match action {
                    ServiceAction::Start => vec![
                        launchctl(["enable".into(), target.clone().into()]),
                        launchctl([
                            "bootstrap".into(),
                            launchd_domain(config).into(),
                            config.definition_path(self.platform)?.into_os_string(),
                        ]),
                        launchctl(["kickstart".into(), target.into()]),
                    ],
                    ServiceAction::Stop => vec![launchctl(["bootout".into(), target.into()])],
                    ServiceAction::Restart => {
                        vec![launchctl(["kickstart".into(), "-k".into(), target.into()])]
                    }
                    ServiceAction::Status => vec![launchctl(["print".into(), target.into()])],
                }
            }
        })
    }

    /// Explicit installation only. The caller first prepares/chowns the managed store and then
    /// invokes this operation. Rendering/inspection never calls it implicitly.
    pub fn install(&self, config: &ServiceConfig, force: bool) -> io::Result<PathBuf> {
        self.ensure_native_platform()?;
        ensure_scope_account(config)?;
        let contents = self.render(config)?;
        let path = config.definition_path(self.platform)?;
        if fs::symlink_metadata(&path).is_ok() && !force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "service definition exists; use --force to replace it",
            ));
        }
        if self.platform == ServicePlatform::Launchd && force {
            let _ = launchctl(["bootout".into(), launchd_target(config).into()]).execute()?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.platform == ServicePlatform::Launchd {
            fs::create_dir_all(&config.log_dir)?;
        }
        write_definition(&path, contents.as_bytes())?;
        match self.platform {
            ServicePlatform::Systemd => {
                systemctl(config, &["daemon-reload"]).checked()?;
                systemctl(config, &["enable", &config.unit_name()]).checked()?;
                systemctl(config, &["restart", &config.unit_name()]).checked()?;
            }
            ServicePlatform::Launchd => {
                self.control(config, ServiceAction::Start)?;
            }
        }
        Ok(path)
    }

    /// Stops/removes the definition; payloads, configuration, credentials, and history are retained.
    pub fn uninstall(&self, config: &ServiceConfig) -> io::Result<()> {
        self.ensure_native_platform()?;
        ensure_scope_account(config)?;
        let path = config.definition_path(self.platform)?;
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "service definition does not exist",
            ));
        }
        match self.platform {
            ServicePlatform::Systemd => {
                systemctl(config, &["disable", "--now", &config.unit_name()]).checked()?;
            }
            ServicePlatform::Launchd => {
                let status =
                    launchctl(["print".into(), launchd_target(config).into()]).execute()?;
                if status.status.success() {
                    launchctl(["bootout".into(), launchd_target(config).into()]).checked()?;
                }
            }
        }
        fs::remove_file(&path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        if self.platform == ServicePlatform::Systemd {
            systemctl(config, &["daemon-reload"]).checked()?;
        }
        Ok(())
    }

    pub fn control(&self, config: &ServiceConfig, action: ServiceAction) -> io::Result<Output> {
        self.ensure_native_platform()?;
        ensure_scope_account(config)?;
        if self.platform == ServicePlatform::Systemd {
            return self.control_commands(config, action)?.remove(0).checked();
        }
        let target = launchd_target(config);
        if matches!(action, ServiceAction::Start | ServiceAction::Restart) {
            launchctl(["enable".into(), target.clone().into()]).checked()?;
            let loaded = launchctl(["print".into(), target.clone().into()])
                .execute()?
                .status
                .success();
            if !loaded {
                launchctl([
                    "bootstrap".into(),
                    launchd_domain(config).into(),
                    config.definition_path(self.platform)?.into_os_string(),
                ])
                .checked()?;
            }
            return if action == ServiceAction::Restart {
                launchctl(["kickstart".into(), "-k".into(), target.into()]).checked()
            } else {
                launchctl(["kickstart".into(), target.into()]).checked()
            };
        }
        self.control_commands(config, action)?.remove(0).checked()
    }

    fn ensure_native_platform(&self) -> io::Result<()> {
        if ServicePlatform::current()? != self.platform {
            return Err(invalid("cannot execute another platform's service manager"));
        }
        Ok(())
    }
}

/// Linux user services require lingering for boot/logout independence. Exposed as an explicit
/// command so the CLI can explain/report a rejected permission request instead of claiming success.
pub fn lingering_command(uid: u32) -> ServiceCommand {
    ServiceCommand::new("loginctl", ["enable-linger".into(), uid.to_string().into()])
}

fn systemctl(config: &ServiceConfig, args: &[&str]) -> ServiceCommand {
    let prefix = if config.scope == ServiceScope::User {
        vec![OsString::from("--user")]
    } else {
        Vec::new()
    };
    ServiceCommand::new(
        "systemctl",
        prefix.into_iter().chain(args.iter().map(OsString::from)),
    )
}
fn launchctl(args: impl IntoIterator<Item = OsString>) -> ServiceCommand {
    ServiceCommand::new("launchctl", args)
}
fn launchd_domain(config: &ServiceConfig) -> String {
    if config.scope == ServiceScope::System {
        "system".into()
    } else {
        format!("gui/{}", config.uid)
    }
}
fn launchd_target(config: &ServiceConfig) -> String {
    format!("{}/{}", launchd_domain(config), config.label)
}

fn ensure_scope_account(config: &ServiceConfig) -> io::Result<()> {
    config.validate()?;
    let uid = unsafe { libc::geteuid() };
    if config.scope == ServiceScope::System && uid != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "system service changes require root",
        ));
    }
    if config.scope == ServiceScope::User && uid != config.uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "user service changes must run as the owning account",
        ));
    }
    Ok(())
}

fn write_definition(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("service definition has no parent"))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()
}

fn path_string(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid("service paths must be UTF-8"))?;
    validate_text(value)?;
    Ok(value.to_owned())
}
fn validate_text(value: &str) -> io::Result<()> {
    if value.chars().any(char::is_control) {
        Err(invalid(
            "control characters are not allowed in service definitions",
        ))
    } else {
        Ok(())
    }
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
// WorkingDirectory is one path, not a shell-like word list: enclosing quotes become
// literal path characters. Internal spaces are literal; percent specifiers still need escaping.
fn systemd_directory(value: &str) -> io::Result<String> {
    if value.trim() != value || value.ends_with('\\') {
        return Err(invalid(
            "systemd working directory cannot end in whitespace or a backslash",
        ));
    }
    Ok(value.replace('%', "%%"))
}
fn systemd_quote(value: &str, exec_argument: bool) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' if exec_argument => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn plist_string(out: &mut String, key: &str, value: &str) {
    out.push_str(&format!(
        "<key>{}</key><string>{}</string>\n",
        xml(key),
        xml(value)
    ));
}
