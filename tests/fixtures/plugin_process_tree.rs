// Standalone Windows test executable, compiled by process/windows_tests.rs.
// No shell or installed scripting runtime is required on the Windows CI runner.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args[1] == "helper" {
        std::thread::sleep(Duration::from_secs(120));
        return;
    }
    if args[1] == "marker" {
        std::fs::write(&args[2], b"started").unwrap();
        return;
    }
    let detached = args[1] == "success" || args[1] == "failure";
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("helper");
    if detached {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    } else {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    let helper = command.spawn().unwrap();
    std::fs::write(&args[2], helper.id().to_string()).unwrap();
    // The test opens a durable process handle before letting the leader exit.
    while !std::path::Path::new(&args[3]).exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    if args[1] == "leader-exits" {
        // The helper keeps the pipes open after its parent exits.
        std::process::exit(0);
    }
    if args[1] == "success" {
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
        std::io::stdout()
            .write_all(br#"{"ok":true,"tracks":[]}"#)
            .unwrap();
        return;
    }
    if args[1] == "failure" {
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
        std::process::exit(7);
    }
    std::thread::sleep(Duration::from_secs(120));
}
