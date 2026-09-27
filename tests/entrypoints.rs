#![cfg(unix)]

use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

struct Plugins(PathBuf);
impl Plugins {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "easymusic-entrypoints-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, artist, id) in [("a", "其他歌手", "t1"), ("b", "周杰伦", "b:t2")] {
            let script = dir.join(format!("easymusic-source-{name}"));
            let response = json!({"ok":true,"tracks":[{"id":id,"title":"晴天","artist":artist}]});
            std::fs::write(
                &script,
                format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{response}'\n"),
            )
            .unwrap();
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(dir)
    }
    fn command(&self) -> Command {
        self.command_with_sources("a,b")
    }
    fn command_with_sources(&self, sources: &str) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_easymusic"));
        // Keep the test independent of user source/runtime preferences.
        for var in [
            "EASYMUSIC_SOURCE",
            "EASYMUSIC_SOURCES",
            "EASYMUSIC_YT_DLP",
            "EASYMUSIC_JS_RUNTIME",
            "EASYMUSIC_STREAM_BIND",
        ] {
            cmd.env_remove(var);
        }
        cmd.arg("--plugins-dir")
            .arg(&self.0)
            .args(["--sources", sources]);
        cmd.kill_on_drop(true);
        cmd
    }
}
impl Drop for Plugins {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn doctor_checks_yt_dlp_only_through_online_youtube_search() {
    let plugins = Plugins::new("doctor");
    let yt_dlp = plugins.0.join("yt-dlp");
    let ffmpeg = plugins.0.join("ffmpeg");
    let marker = plugins.0.join("yt-dlp-called");
    for (path, script) in [
        (
            &yt_dlp,
            "#!/bin/sh\nprintf '%s\\n' called >> \"$0-called\"\necho unavailable >&2\nexit 1\n",
        ),
        (&ffmpeg, "#!/bin/sh\necho 'ffmpeg test'\n"),
    ] {
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    for (sources, args, checked, ok, search_sources) in [
        ("youtube,b", vec![], false, true, vec![]),
        (
            "youtube,b",
            vec!["--source", "youtube"],
            false,
            true,
            vec![],
        ),
        (
            "youtube,b",
            vec!["--online", "--source", "b"],
            false,
            true,
            vec!["b"],
        ),
        ("b", vec!["--online"], false, true, vec!["b"]),
        (
            "youtube,b",
            vec!["--online"],
            true,
            true,
            vec!["youtube", "b"],
        ),
        (
            "youtube,b",
            vec!["--online", "--source", "youtube"],
            true,
            false,
            vec!["youtube"],
        ),
    ] {
        let _ = std::fs::remove_file(&marker);
        let output = plugins
            .command_with_sources(sources)
            .arg("--yt-dlp")
            .arg(&yt_dlp)
            .args(["doctor", "--ffmpeg"])
            .arg(&ffmpeg)
            .args(&args)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], ok, "{args:?}: {value}");
        assert_eq!(marker.exists(), checked, "{args:?}: {value}");
        assert!(value.get("yt_dlp").is_none(), "{value}");
        if checked {
            // Only the search invokes yt-dlp; there is no separate version probe.
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "called\n");
            let youtube = value["search"]
                .as_array()
                .unwrap()
                .iter()
                .find(|status| status["source"] == "youtube")
                .unwrap();
            assert_eq!(youtube["ok"], false);
            assert!(youtube["error"].as_str().unwrap().contains("unavailable"));
        }
        if search_sources.is_empty() {
            assert!(value["search"].is_null());
        } else {
            let actual = value["search"]
                .as_array()
                .unwrap()
                .iter()
                .map(|status| status["source"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(actual, search_sources);
        }
    }
}

#[tokio::test]
async fn cli_merge_preserves_artist_ranking_and_plugin_wire_ids() {
    let plugins = Plugins::new("cli");
    let output = plugins
        .command()
        .args([
            "search",
            "--keyword",
            "晴天",
            "--artist",
            "周杰伦",
            "--all-sources",
            "--limit",
            "1",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        json!({"ok":true,"keyword":"晴天 周杰伦","source":"all","count":1,"tracks":[{"id":"b:t2","title":"晴天","artist":"周杰伦"}]})
    );
    let output = plugins
        .command()
        .args([
            "search",
            "--keyword",
            "晴天",
            "--all-sources",
            "--limit",
            "0",
        ])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[tokio::test]
async fn mcp_uses_startup_source_and_allows_tool_override() {
    let plugins = Plugins::new("mcp");
    let mut child = plugins
        .command()
        .args(["--source", "b", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    async fn send(input: &mut tokio::process::ChildStdin, value: Value) {
        input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    send(&mut input, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
    tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    send(
        &mut input,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    for (id, source, expected) in [(2, None, "b:t2"), (3, Some("a"), "a:t1")] {
        let mut arguments = json!({"title":"晴天","artist":"周杰伦"});
        if let Some(source) = source {
            arguments["source"] = source.into();
        }
        send(&mut input, json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"search_music","arguments":arguments}})).await;
        let response = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        let result: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(result["selected"]["id"], expected);
        assert_eq!(result["needs_confirmation"], source.is_some());
    }
    child.kill().await.unwrap();
}
