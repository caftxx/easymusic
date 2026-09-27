use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::OnceLock;
use std::time::Instant;

use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

use super::*;
use std::path::{Path, PathBuf};

fn fixture_executable() -> &'static Path {
    static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
    EXECUTABLE.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("easymusic-windows-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("plugin.exe");
        let output =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg("--edition=2024")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/plugin_process_tree.rs"),
                )
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("compile native plugin fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    })
}

struct Fixture {
    dir: PathBuf,
    command: Command,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let executable = fixture_executable();
        let dir =
            std::env::temp_dir().join(format!("easymusic-windows-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Remove stale synchronization files if a test is rerun in the same process.
        let _ = std::fs::remove_file(dir.join("pid"));
        let _ = std::fs::remove_file(dir.join("go"));
        let mut command = Command::new(executable);
        command.arg(tag).arg(dir.join("pid")).arg(dir.join("go"));
        Self { dir, command }
    }

    async fn helper(&self) -> TestProcess {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(value) = std::fs::read_to_string(self.dir.join("pid"))
                && let Ok(pid) = value.parse::<u32>()
            {
                // Safety: open a non-inheritable handle to the helper created
                // by this fixture, kept alive until the test releases its gate.
                let handle =
                    unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
                assert!(!handle.is_null(), "helper must be alive before cleanup");
                // Safety: OpenProcess returned an owned, valid handle.
                return TestProcess(unsafe { OwnedHandle::from_raw_handle(handle) });
            }
            assert!(Instant::now() < deadline, "fixture helper never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn release(&self) {
        std::fs::write(self.dir.join("go"), b"go").unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct TestProcess(OwnedHandle);

impl TestProcess {
    async fn assert_terminated(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // Safety: the handle stays alive for the whole wait. Unlike a PID
            // lookup it cannot mistake a recycled process ID for this helper.
            let state = unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) };
            if state == WAIT_OBJECT_0 {
                return;
            }
            assert_eq!(state, WAIT_TIMEOUT, "waiting for helper failed");
            assert!(
                Instant::now() < deadline,
                "plugin helper survived request cleanup"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for TestProcess {
    fn drop(&mut self) {
        // Safety: this test owns the helper. Also clean up on assertion failure.
        unsafe {
            TerminateProcess(self.0.as_raw_handle(), 1);
        }
    }
}

#[tokio::test]
async fn plugin_cannot_run_before_job_assignment() {
    let fixture = Fixture::new("marker");
    let mut command = Command::new(fixture_executable());
    command.arg("marker").arg(fixture.dir.join("pid"));
    let mut guard = ProcessTreeGuard::new().unwrap();
    let mut child = guard.spawn(&mut command).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!fixture.dir.join("pid").exists());
    guard.attach(&child).unwrap();
    assert!(
        timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(fixture.dir.join("pid").exists());
    // A reaped child cannot be attached; failure must be reported, not ignored.
    assert!(guard.attach(&child).is_err());
}

async fn run_tree_case(mode: &str) {
    let mut fixture = Fixture::new(mode);
    let mut command = std::mem::replace(&mut fixture.command, Command::new("unused"));
    let task = tokio::spawn(async move {
        run(
            &mut command,
            vec![b'x'; 1024 * 1024],
            Duration::from_secs(5),
        )
        .await
    });
    let helper = fixture.helper().await;
    fixture.release();
    if mode == "cancel" {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    } else {
        let result = task.await.unwrap();
        match mode {
            "success" => assert_eq!(result.unwrap(), br#"{"ok":true,"tracks":[]}"#),
            "failure" => assert!(result.unwrap_err().to_string().contains("exited")),
            _ => assert!(result.unwrap_err().to_string().contains("timed out")),
        }
    }
    helper.assert_terminated().await;
}

#[tokio::test]
async fn timeout_terminates_descendants() {
    run_tree_case("timeout").await;
}

#[tokio::test]
async fn cancellation_terminates_descendants() {
    run_tree_case("cancel").await;
}

#[tokio::test]
async fn cleanup_survives_the_leader_exiting_first() {
    run_tree_case("leader-exits").await;
}

#[tokio::test]
async fn success_terminates_detached_helpers() {
    run_tree_case("success").await;
}

#[tokio::test]
async fn failure_terminates_detached_helpers() {
    run_tree_case("failure").await;
}
