//! Request-scoped subprocess execution. No music models or plugin JSON live here.
use std::{
    io,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    time::timeout,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessError {
    #[error("failed to start process: {0}")]
    Spawn(io::Error),
    #[error("failed to contain or resume process: {0}")]
    Setup(io::Error),
    #[error("process I/O failed: {0}")]
    Io(io::Error),
    #[error("timed out after {}", describe_duration(*.0))]
    TimedOut(Duration),
    #[error("exited with {status}: {detail}")]
    Exit { status: ExitStatus, detail: String },
}

fn describe_duration(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        return format!("{} milliseconds", duration.as_millis());
    }
    format!("{} seconds", duration.as_secs())
}

#[cfg(windows)]
mod windows;
#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(all(test, unix))]
mod tests;

/// Owns the request's process group/job through every return and cancellation.
struct ProcessTreeGuard {
    #[cfg(unix)]
    pgid: Option<u32>,
    #[cfg(windows)]
    job: windows::Job,
}

impl ProcessTreeGuard {
    fn new() -> io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            pgid: None,
            #[cfg(windows)]
            job: windows::Job::new()?,
        })
    }

    fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        command.spawn()
    }

    fn attach(&mut self, child: &Child) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.pgid = child.id();
        }
        #[cfg(windows)]
        {
            self.job.assign(child)?;
            windows::resume(child)?;
        }
        Ok(())
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.and_then(|pgid| libc::pid_t::try_from(pgid).ok()) {
            // Safety: the group was created for this plugin. Never signal the
            // host's group, even if process-group setup were to fail.
            if pgid != unsafe { libc::getpgrp() } {
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
        // Windows: OwnedHandle closes the job, terminating all its members.
    }
}

/// Containment must exist before the plugin can execute its first instruction.
fn spawn_plugin(command: &mut Command) -> Result<(Child, ProcessTreeGuard), ProcessError> {
    let mut cleanup = ProcessTreeGuard::new().map_err(ProcessError::Setup)?;
    let child = cleanup.spawn(command).map_err(ProcessError::Spawn)?;
    // On failure the child is still suspended (or already in the job). Drop
    // both owners and report the error instead of running an uncontained plugin.
    cleanup.attach(&child).map_err(ProcessError::Setup)?;
    Ok((child, cleanup))
}

/// Run one byte request and collect stdout, owning the whole process tree.
pub(crate) async fn run(
    command: &mut Command,
    payload: Vec<u8>,
    deadline: Duration,
) -> Result<Vec<u8>, ProcessError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, _cleanup) = spawn_plugin(command)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");

    // Feeding stdin and draining the output run as two halves of *one*
    // future: a plugin that ignores its input (or reads it only after
    // writing a lot of output) cannot wedge us in `write_all`, because
    // either half keeps the other progressing. No detached task exists, so
    // cancelling this future also drops the blocked write together with
    // the request payload instead of leaking them.
    let exchange = async move {
        let write = async move {
            stdin.write_all(&payload).await?;
            stdin.flush().await?;
            // Closing stdin is the EOF signal for plugins that read to end.
            drop(stdin);
            Ok::<(), io::Error>(())
        };
        let read = child.wait_with_output();
        tokio::join!(write, read)
    };

    let (write_result, status_output) = match timeout(deadline, exchange).await {
        Err(_) => {
            // The dropped future closed stdin and `kill_on_drop` terminated
            // the plugin leader; `cleanup` takes whatever the plugin
            // spawned as this function returns.
            return Err(ProcessError::TimedOut(deadline));
        }
        Ok(pair) => pair,
    };
    let output = status_output.map_err(ProcessError::Io)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned();
        return Err(ProcessError::Exit {
            status: output.status,
            detail,
        });
    }
    write_result.map_err(ProcessError::Io)?;
    Ok(output.stdout)
}
