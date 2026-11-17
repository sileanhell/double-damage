//! A Windows Job Object that every child process joins. Its handle is never closed, so when the
//! app exits for any reason (including a crash or being killed from Task Manager) Windows closes
//! it and kills the children: a model server can never be left holding video memory.

use std::sync::OnceLock;

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
                TerminateJobObject,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    },
    core::PCWSTR,
};

/// The job handle as an integer (`HANDLE` itself is not `Send`).
static JOB: OnceLock<Option<isize>> = OnceLock::new();

fn job() -> Option<HANDLE> {
    let raw = *JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls; `info` outlives the call that reads it.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            Some(job.0 as isize)
        }
    });
    raw.map(|raw| HANDLE(raw as *mut std::ffi::c_void))
}

/// A process tree of its own: a child and everything it starts. `kill` ends them all; so does
/// dropping it, and the app exiting (the handle closes with the process).
///
/// To have no window in which the child can start something outside the job, create it with
/// `CREATE_SUSPENDED`, [`Tree::adopt`] it, then [`resume`] it.
pub struct Tree(isize);

impl Tree {
    pub fn adopt(child: &tokio::process::Child) -> Option<Self> {
        let process = child.raw_handle()?;
        // SAFETY: plain Win32 calls on handles that are valid for their duration.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let tree = Self(job.0 as isize);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .ok()?;
            AssignProcessToJobObject(job, HANDLE(process)).ok()?;
            Some(tree)
        }
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut std::ffi::c_void)
    }

    pub fn kill(&self) {
        // SAFETY: the handle is ours until drop.
        let _ = unsafe { TerminateJobObject(self.handle(), 1) };
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // SAFETY: closed exactly once; with KILL_ON_JOB_CLOSE this ends what's left.
        let _ = unsafe { CloseHandle(self.handle()) };
    }
}

// SAFETY: a job handle can be used from any thread.
unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}

/// Resumes the threads of a child created with `CREATE_SUSPENDED` (std/tokio don't expose the
/// primary thread handle, so it is found by a thread snapshot). Fails if no thread could be
/// resumed; the caller should then kill the child.
pub fn resume(child: &tokio::process::Child) -> Result<(), String> {
    let pid = child.id().ok_or("the process has already exited")?;
    // SAFETY: plain Win32 calls; every handle opened here is closed here.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(|e| format!("cannot list threads: {e}"))?;
        let mut entry = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut resumed = 0;
        let mut more = Thread32First(snapshot, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid
                && let Ok(thread) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
            {
                if ResumeThread(thread) != u32::MAX {
                    resumed += 1;
                }
                let _ = CloseHandle(thread);
            }
            more = Thread32Next(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        if resumed == 0 { Err("cannot resume the new process".into()) } else { Ok(()) }
    }
}

/// Ties a child process to the app's lifetime. Best effort: on failure the child still runs and
/// is stopped on a normal exit.
pub fn adopt_std(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    let Some(job) = job() else { return };
    // SAFETY: both handles are valid for the duration of the call.
    let _ = unsafe { AssignProcessToJobObject(job, HANDLE(child.as_raw_handle())) };
}
