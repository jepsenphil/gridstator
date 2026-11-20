use super::{actions::ActLogParser, emit, system};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
#[path = "windows.rs"]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    Timeout,
}
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub stop: Option<StopReason>,
    pub cancelled_at: Option<i64>,
}

pub enum StopAction {
    Interrupt(u64),
    Container(String),
}

pub enum OutputMode {
    Job,
    /// Docker setup stdout is an opaque resource ID; setup diagnostics belong in system logs.
    Setup,
}

pub struct Run<'a> {
    pub bin: &'a str,
    pub stdin: Option<std::fs::File>,
    pub args: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    pub cwd: Option<&'a Path>,
    pub timeout: Option<u64>,
    pub phase_running: bool,
    pub stop_action: StopAction,
    pub act_parser: Option<Arc<Mutex<ActLogParser>>>,
    pub output_mode: OutputMode,
}

/// Wait for the actual child and both output readers. Cancellation signals the workload and
/// reaps it; dropping a future is never used as a substitute for stopping external resources.
pub async fn run(
    options: Run<'_>,
    cancel: &CancellationToken,
    logs: &mpsc::Sender<(String, String)>,
) -> Result<ProcessResult> {
    let mut command = Command::new(options.bin);
    command
        .args(options.args)
        .envs(options.env)
        .stdin(options.stdin.map_or_else(Stdio::null, Stdio::from))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    #[cfg(windows)]
    let job = {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        windows::Job::new()?
    };
    if let Some(cwd) = options.cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not start {}", options.bin))?;
    #[cfg(windows)]
    if let Err(error) = job.start(&child) {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(error.context("Could not contain workload in a Windows job"));
    }
    let stdout = child.stdout.take().context("child stdout missing")?;
    let stderr = child.stderr.take().context("child stderr missing")?;
    let (stdout_stream, stderr_stream) = match options.output_mode {
        OutputMode::Job => ("stdout", "stderr"),
        OutputMode::Setup => ("discard", "system"),
    };
    let out = tokio::spawn(pump(
        stdout,
        stdout_stream,
        logs.clone(),
        options.act_parser.clone(),
    ));
    let err = tokio::spawn(pump(
        stderr,
        stderr_stream,
        logs.clone(),
        options.act_parser,
    ));
    if options.phase_running {
        emit(logs, "phase", "running".into()).await;
    }
    let deadline = async {
        match options.timeout {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);
    let mut stop = None;
    let mut cancelled_at = None;
    let status: ExitStatus = tokio::select! {
        biased;
        status = child.wait() => status?,
        _ = cancel.cancelled() => {
            stop = Some(StopReason::Cancelled);
            cancelled_at = Some(chrono::Utc::now().timestamp_millis());
            stop_child(&mut child, &options.stop_action, logs, "Cancelling: stopping the process", #[cfg(windows)] &job).await?
        }
        _ = &mut deadline => {
            stop = Some(StopReason::Timeout);
            stop_child(&mut child, &options.stop_action, logs, &format!("Timed out after {}s; stopping", options.timeout.unwrap_or(0)), #[cfg(windows)] &job).await?
        }
    };
    // A tool may exit while its descendants retain the inherited log pipe handles. Closing
    // the job before waiting for readers terminates those descendants and closes their pipes.
    #[cfg(windows)]
    drop(job);
    // Readers keep consuming through graceful termination and final pipe close.
    out.await.context("stdout reader task failed")??;
    err.await.context("stderr reader task failed")??;
    Ok(ProcessResult {
        exit_code: status.code(),
        stop,
        cancelled_at,
    })
}

async fn stop_child(
    child: &mut tokio::process::Child,
    action: &StopAction,
    logs: &mpsc::Sender<(String, String)>,
    message: &str,
    #[cfg(windows)] job: &windows::Job,
) -> Result<ExitStatus> {
    system(logs, message).await;
    match action {
        StopAction::Interrupt(_) => {
            #[cfg(unix)]
            if let Some(id) = child.id() {
                // SAFETY: this owned child's PID identifies its dedicated process group.
                unsafe {
                    libc::kill(-(id as libc::pid_t), libc::SIGINT);
                }
            }
            #[cfg(windows)]
            job.terminate()?;
            #[cfg(not(any(unix, windows)))]
            {
                child.start_kill()?;
            }
        }
        StopAction::Container(name) => {
            let mut stop = Command::new(super::docker_binary());
            stop.args(["stop", "--time", "10", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            // Run independently of log pipes: docker stop, rather than killing attach, stops
            // the init process and its process tree inside the container.
            let _ = tokio::time::timeout(Duration::from_secs(15), stop.status()).await;
        }
    }
    let grace = match action {
        StopAction::Interrupt(seconds) => *seconds,
        StopAction::Container(_) => 15,
    };
    match tokio::time::timeout(Duration::from_secs(grace), child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_) => {
            #[cfg(windows)]
            job.terminate()?;
            #[cfg(unix)]
            if let Some(id) = child.id() {
                // SAFETY: the child's dedicated process group has the child's PID.
                unsafe {
                    libc::kill(-(id as libc::pid_t), libc::SIGKILL);
                }
            }
            child.start_kill()?;
            Ok(child.wait().await?)
        }
    }
}

async fn pump(
    mut input: impl AsyncRead + Unpin,
    stream: &'static str,
    logs: mpsc::Sender<(String, String)>,
    parser: Option<Arc<Mutex<ActLogParser>>>,
) -> Result<()> {
    if stream == "discard" {
        tokio::io::copy(&mut input, &mut tokio::io::sink()).await?;
        return Ok(());
    }
    let mut bytes = Vec::new();
    let mut text = String::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut flush = tokio::time::interval(Duration::from_millis(100));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            count = input.read(&mut buffer) => {
                let count = count?;
                if count == 0 {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                    if !text.is_empty() { deliver(&logs, stream, &text, parser.as_ref()).await; }
                    return Ok(());
                }
                bytes.extend_from_slice(&buffer[..count]);
                decode(&mut bytes, &mut text);
                while let Some(index) = text.find('\n') {
                    let line: String = text.drain(..=index).collect();
                    deliver(&logs, stream, &line, parser.as_ref()).await;
                }
                // act JSON is one record per line; a partial record must never be parsed as
                // ordinary text. Its record limit prevents unbounded tool-controlled memory.
                if text.len() > 1024 * 1024 || (parser.is_none() && text.len() > 8192) {
                    deliver(&logs, stream, &text, parser.as_ref()).await; text.clear();
                }
            }
            _ = flush.tick(), if parser.is_none() && !text.is_empty() => {
                deliver(&logs, stream, &text, None).await; text.clear();
            }
        }
    }
}

fn decode(bytes: &mut Vec<u8>, text: &mut String) {
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                text.push_str(valid);
                bytes.clear();
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                text.push_str(
                    std::str::from_utf8(&bytes[..valid]).expect("validated UTF-8 prefix"),
                );
                bytes.drain(..valid);
                if let Some(invalid) = error.error_len() {
                    text.push('\u{fffd}');
                    bytes.drain(..invalid);
                } else {
                    return;
                }
            }
        }
    }
}

async fn deliver(
    logs: &mpsc::Sender<(String, String)>,
    stream: &str,
    text: &str,
    parser: Option<&Arc<Mutex<ActLogParser>>>,
) {
    if let Some(parser) = parser {
        let (lines, outputs) = {
            let mut parser = parser
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let lines = parser.feed(text, stream);
            let outputs = if parser.take_changed() {
                Some(serde_json::json!({"steps":parser.steps()}))
            } else {
                None
            };
            (lines, outputs)
        };
        for (stream, text) in lines {
            emit(logs, &stream, text).await;
        }
        if let Some(outputs) = outputs {
            emit(logs, "outputs", outputs.to_string()).await;
        }
    } else {
        emit(logs, stream, text.to_owned()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn line_pump_reassembles_split_utf8_and_flushes_partial_at_eof() {
        let (mut write, read) = tokio::io::duplex(64);
        let (send, mut receive) = mpsc::channel(16);
        let parser = Arc::new(Mutex::new(ActLogParser::default()));
        let task = tokio::spawn(pump(read, "stdout", send, Some(parser)));
        let bytes = "héllo\nwörld\npartial".as_bytes();
        for chunk in [&bytes[..2], &bytes[2..9], &bytes[9..]] {
            write.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
        drop(write);
        task.await.unwrap().unwrap();
        let mut entries = Vec::new();
        while let Some(entry) = receive.recv().await {
            entries.push(entry);
        }
        assert_eq!(
            entries,
            [
                ("stdout".into(), "héllo\n".into()),
                ("stdout".into(), "wörld\n".into()),
                ("stdout".into(), "partial\n".into())
            ]
        );
    }

    /// Find a grandchild of this test process by image name: workload (cmd.exe) -> `image`.
    #[cfg(windows)]
    fn windows_grandchild(image: &str) -> Option<u32> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
        };
        // SAFETY: a process snapshot has no borrowed inputs; the handle is checked and owned.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut processes = Vec::new();
        // SAFETY: entry is initialised with its size and outlives each synchronous call.
        let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            processes.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
            found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
        }
        let this = std::process::id();
        processes.iter().find_map(|(pid, parent, name)| {
            let workload = processes
                .iter()
                .any(|(candidate, grandparent, _)| candidate == parent && *grandparent == this);
            (workload && name.eq_ignore_ascii_case(image)).then_some(*pid)
        })
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_cancellation_and_timeout_reap_descendants_and_close_log_pipes() {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        for timed_out in [false, true] {
            // cmd.exe and ping.exe start in milliseconds even on Windows ARM64 runners, where a
            // cold PowerShell took over 30s. ping inherits the workload's log pipe handles, so
            // run() can only return once the job has terminated this descendant too.
            let args = vec!["/d".into(), "/c".into(), "ping.exe -n 60 127.0.0.1".into()];
            let (send, mut receive) = mpsc::channel(128);
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let task = tokio::spawn(async move {
                run(
                    Run {
                        bin: "cmd.exe",
                        stdin: None,
                        args: &args,
                        env: &BTreeMap::new(),
                        cwd: None,
                        timeout: timed_out.then_some(5),
                        phase_running: false,
                        stop_action: StopAction::Interrupt(1),
                        act_parser: None,
                        output_mode: OutputMode::Job,
                    },
                    &token,
                    &send,
                )
                .await
            });
            // ping's first output proves the descendant exists and holds the log pipe.
            tokio::time::timeout(Duration::from_secs(30), async {
                while let Some((_, line)) = receive.recv().await {
                    if line.contains("127.0.0.1") {
                        return;
                    }
                }
                panic!("workload exited before spawning a descendant");
            })
            .await
            .expect("descendant produced no output");
            let pid = windows_grandchild("PING.EXE").expect("ping.exe descendant not found");
            // Hold a process handle so termination can be checked without PID reuse.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            assert!(!handle.is_null());
            if !timed_out {
                cancel.cancel();
            }
            let drain = tokio::spawn(async move { while receive.recv().await.is_some() {} });
            let result = tokio::time::timeout(Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            drain.await.unwrap();
            assert_eq!(
                result.stop,
                Some(if timed_out {
                    StopReason::Timeout
                } else {
                    StopReason::Cancelled
                })
            );
            let mut exit_code = STILL_ACTIVE as u32;
            let inspected = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
            unsafe {
                CloseHandle(handle);
            }
            assert_ne!(inspected, 0);
            assert_ne!(
                exit_code, STILL_ACTIVE as u32,
                "descendant survived stopping its workload"
            );
        }
    }
}
