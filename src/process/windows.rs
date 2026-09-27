//! Windows process containment. The plugin must remain suspended until it is
//! assigned to the job; assigning an already running process misses children
//! it created before the assignment.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use tokio::process::Child;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

pub(super) struct Job(OwnedHandle);

impl Job {
    pub(super) fn new() -> io::Result<Self> {
        // Safety: the unnamed, non-inheritable job is owned only by this guard.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Safety: CreateJobObjectW returned a valid handle with unique ownership.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // Safety: the buffer has the layout and size required by this class.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn assign(&self, child: &Child) -> io::Result<()> {
        // Borrow the original process handle instead of reopening by PID. It
        // remains valid even if the process exits and cannot identify a reused PID.
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("plugin process handle is unavailable"))?;
        // Safety: both handles remain owned and live throughout this call.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Tokio/std closes the primary thread handle when spawning. Find that thread
/// while the process is still suspended, using the documented Tool Help API.
/// This avoids nightly std process attributes and preserves Rust 1.88 support.
pub(super) fn resume(child: &Child) -> io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("suspended plugin has exited"))?;
    // Safety: snapshot creation has no pointer arguments. This snapshot covers
    // all threads; the loop below restricts it to our suspended child.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // Safety: the successful snapshot call transferred ownership of this handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // Safety: entry is initialized and advertises its actual size.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32OwnerProcessID == pid {
            // The primary thread has not run, so the plugin cannot create any
            // other threads or descendants before this point.
            // Safety: the snapshot provides the child thread's ID.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // Safety: OpenThread transferred ownership of a valid handle.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // Safety: this thread belongs to the suspended child already in our job.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // Safety: snapshot and entry remain valid for the next iteration.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
    Err(io::Error::other("suspended plugin thread was not found"))
}
