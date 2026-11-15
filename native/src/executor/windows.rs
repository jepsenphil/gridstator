//! Put each external workload in a kill-on-close job before its first instruction runs.
use anyhow::{Context, Result, bail};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

pub(super) struct Job(OwnedHandle);

impl Job {
    pub(super) fn new() -> Result<Self> {
        // SAFETY: unnamed job has no borrowed attributes; the returned owned handle is checked.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits has the exact layout and lifetime required by this synchronous call.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(job)
    }

    pub(super) fn start(&self, child: &tokio::process::Child) -> Result<()> {
        let process = child
            .raw_handle()
            .context("Suspended process handle missing")?;
        let pid = child.id().context("Suspended process identity missing")?;
        // SAFETY: both handles remain live; the child was created suspended and cannot spawn
        // descendants until it is assigned to this job and its primary thread resumes.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut thread: THREADENTRY32 = unsafe { std::mem::zeroed() };
        thread.dwSize = std::mem::size_of_val(&thread) as u32;
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut thread) };
        while found != 0 {
            if thread.th32OwnerProcessID == pid {
                let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread.th32ThreadID) };
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
                if unsafe { ResumeThread(handle.as_raw_handle()) } == u32::MAX {
                    return Err(std::io::Error::last_os_error().into());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut thread) };
        }
        bail!("Could not find the suspended workload thread")
    }

    pub(super) fn terminate(&self) -> Result<()> {
        // SAFETY: the job handle is owned and remains valid for this synchronous call.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
