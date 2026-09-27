//! Embeds cargo-style version metadata (git short hash + commit date) into
//! `--version` output. Falls back to the plain crate version when git info is
//! unavailable (e.g. building from a source tarball).

use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!("cargo::rerun-if-changed=.git/");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let Some((hash, date)) = git_metadata() else {
        println!("cargo::rustc-env=EASYMUSIC_VERSION={version}");
        return;
    };
    println!("cargo::rustc-env=EASYMUSIC_VERSION={version} ({hash} {date})");
}

fn git_metadata() -> Option<(String, String)> {
    let hash = git(&["rev-parse", "--short", "HEAD"])?;
    let date = git(&["log", "-1", "--format=%cd", "--date=short"])?;
    Some((hash, date))
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let stdout = stdout.trim().to_owned();
    (!stdout.is_empty()).then_some(stdout)
}

fn manifest_dir() -> std::path::PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}
