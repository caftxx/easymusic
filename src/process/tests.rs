use super::*;
use std::path::{Path, PathBuf};

struct TestRunner {
    executable: PathBuf,
    deadline: Duration,
}
impl TestRunner {
    fn new(executable: PathBuf) -> Self {
        Self::with_timeout(executable, Duration::from_secs(60))
    }
    fn with_timeout(executable: PathBuf, deadline: Duration) -> Self {
        Self {
            executable,
            deadline,
        }
    }
    async fn exchange(&self, payload: &str) -> Result<Vec<u8>, ProcessError> {
        run(
            &mut Command::new(&self.executable),
            payload.as_bytes().to_vec(),
            self.deadline,
        )
        .await
    }
}

/// A plugin that never reads stdin cannot wedge us in `write_all`: the
/// deadline covers feeding the request *and* waiting for the output.
#[cfg(unix)]
#[tokio::test]
async fn wedged_plugin_hits_the_deadline_instead_of_hanging() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("easymusic-wedge-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Ignores stdin entirely and outlives the deadline.
    let script = dir.join("helper-wedge");
    std::fs::write(&script, b"#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let descriptor = script;
    let source = TestRunner::with_timeout(descriptor, Duration::from_millis(250));
    // Larger than the pipe buffer, so a serialised write would block here.
    let keyword = "x".repeat(1024 * 1024);

    let started = std::time::Instant::now();
    let error = source
        .exchange(&keyword)
        .await
        .expect_err("wedged plugin must fail");
    let elapsed = started.elapsed();

    assert!(matches!(error, ProcessError::TimedOut(_)));
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(
        elapsed < Duration::from_secs(10),
        "deadline was not enforced: {elapsed:?}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A plugin that spawns a helper inheriting its pipes must not leave that
/// helper behind when the deadline fires: it would keep the pipes (and the
/// abandoned request payload) alive forever.
///
/// Regression: the writer used to be a detached task, and only the plugin
/// leader was killed, so the helper survived with PID 1 as its parent.
#[cfg(unix)]
#[tokio::test]
async fn timeout_terminates_the_plugin_process_tree() {
    let (dir, pid_file, descriptor) = spawn_helper_plugin("tree-timeout");
    // Long enough that the helper is certainly running before the deadline
    // fires, so the test observes cleanup of a live process tree rather
    // than the plugin being killed off before it forked anything.
    let source = std::sync::Arc::new(TestRunner::with_timeout(descriptor, Duration::from_secs(3)));

    let task = tokio::spawn({
        let source = std::sync::Arc::clone(&source);
        async move {
            // Larger than the pipe buffer, so the write stays blocked for
            // the whole deadline.
            source.exchange(&"x".repeat(1024 * 1024)).await
        }
    });
    let helper = wait_for_pid_file(&pid_file)
        .await
        .expect("plugin helper never started");

    assert!(
        wait_for_descendant_exit(helper).await,
        "plugin helper {helper} survived the timeout"
    );
    let error = task
        .await
        .expect("request task panicked")
        .expect_err("wedged plugin must time out");
    assert!(error.to_string().contains("timed out"), "{error}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The same cleanup must happen when the caller cancels the request, which
/// is the path a dropped MCP tool call takes.
#[cfg(unix)]
#[tokio::test]
async fn cancelled_request_terminates_the_plugin_process_tree() {
    let (dir, pid_file, descriptor) = spawn_helper_plugin("tree-cancel");
    // The default deadline is far longer than this test, so only the
    // cancellation can clean up.
    let source = std::sync::Arc::new(TestRunner::new(descriptor));

    let task = tokio::spawn({
        let source = std::sync::Arc::clone(&source);
        async move { source.exchange(&"x".repeat(1024 * 1024)).await }
    });
    let helper = wait_for_pid_file(&pid_file)
        .await
        .expect("plugin helper never started");
    task.abort();

    assert!(
        wait_for_descendant_exit(helper).await,
        "plugin helper {helper} survived cancellation"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Plugins are request-scoped: even a plugin that answered successfully has
/// its whole process tree terminated once the request ends, so a long-lived
/// server cannot accumulate leftovers.
#[cfg(unix)]
#[tokio::test]
async fn successful_request_terminates_its_process_tree_too() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("easymusic-tree-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("helper.pid");
    let script = dir.join("helper-keep");
    // The helper detaches from the pipes, so the request can complete while
    // the helper is still there to be observed afterwards.
    std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 300 >/dev/null 2>&1 & echo $! > '{}'\ncat > /dev/null\nprintf '%s' '{{\"ok\":true,\"tracks\":[]}}'\n",
                pid_file.display()
            ),
        )
        .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let source = TestRunner::new(script);
    // Start recording during the request, so the assertion below can tell
    // "terminated" apart from "never started".
    let helper = wait_for_pid_file(&pid_file);
    let result = source
        .exchange("request")
        .await
        .expect("plugin should answer");
    assert_eq!(result, br#"{"ok":true,"tracks":[]}"#);

    let helper = helper.await.expect("plugin helper never started");
    assert!(
        wait_for_descendant_exit(helper).await,
        "plugin helper {helper} outlived its request"
    );
    // Safety: belt and braces for the helper this test started.
    unsafe {
        libc::kill(helper, libc::SIGKILL);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write a plugin that starts a pipe-inheriting `sleep` helper, records the
/// helper's pid, then hangs without ever reading its input.
#[cfg(unix)]
fn spawn_helper_plugin(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("easymusic-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("helper.pid");
    let script = dir.join(format!("helper-{tag}"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep 300 & echo $! > '{}'\nsleep 300\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, pid_file, script)
}

#[cfg(unix)]
async fn wait_for_pid_file(path: &Path) -> Option<i32> {
    // Generous: these tests run in parallel with the rest of the suite, and
    // process startup can stall under that load.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            return Some(pid);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Truthful only for a live process: spawning `ps` here would make the
/// check depend on process limits, so signal 0 is used instead (it performs
/// error checking without delivering anything). `EPERM` still proves the
/// pid exists.
#[cfg(unix)]
fn descendant_is_running(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn wait_for_descendant_exit(pid: i32) -> bool {
    // Generous: the killed helper is reaped by the init process, which can
    // lag while the rest of the suite is loading the machine.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if !descendant_is_running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
