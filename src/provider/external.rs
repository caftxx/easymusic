//! External music-source plugins.
//!
//! Any executable named `easymusic-source-<name>` found in a plugin
//! directory (or on `PATH` when `EASYMUSIC_SOURCE_PATH` is set) is loaded as
//! a source plugin. Plugins speak a small JSON-over-stdio subprocess
//! protocol so they can be written in any language:
//!
//! Request (stdin):
//!   `{"protocol":"easymusic-source/1","operation":"search","source":"<name>","keyword":"...","limit":10}`
//!   `{"protocol":"easymusic-source/1","operation":"resolve","source":"<name>","id":"<native id>"}`
//!
//! Response (stdout):
//!   search:  `{"ok":true,"tracks":[{"id":"...","title":"...","artist":"...","artwork_url":"..."}]}`
//!   resolve: `{"ok":true,"url":"https://...","title":"...","extension":"mp3"}`
//!   failure: `{"ok":false,"error":{"message":"..."}}`
//!
//! Search IDs are the plugin's native IDs; easymusic namespaces them as
//! `"<name>:<id>"` before handing them to callers and strips that prefix
//! again before calling `resolve`. A plugin may also emit already namespaced
//! IDs, which are recognised and left untouched.
//!
//! Non-zero exit codes and malformed JSON are reported as source errors, and
//! one [`PLUGIN_TIMEOUT`] deadline covers feeding the request, running the
//! plugin, and draining its output, so a wedged plugin still fails over.
//!
//! A plugin's process tree is request-scoped: when the request ends — answer,
//! error, deadline, or the caller cancelling it — everything the plugin
//! started is terminated, on every platform. Plugins must therefore not keep
//! background processes alive between requests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;

use crate::error::{EasyMusicError, ErrorCode, Result};
use crate::network::validate_http_url;
use crate::provider::{MusicSource, non_empty};
use crate::provider::{SourceDiagnostics, SourceResolvedTrack, SourceTrack};
use crate::track_id::NativeTrackId;

pub const PROTOCOL_VERSION: &str = "easymusic-source/1";
pub const PLUGIN_PREFIX: &str = "easymusic-source-";
/// Overall deadline for one plugin request: it covers feeding stdin, waiting
/// for the process, and draining its output.
pub const PLUGIN_TIMEOUT: Duration = Duration::from_secs(60);

/// A discovered external plugin: source name plus executable path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    pub name: String,
    pub executable: PathBuf,
}

/// Environment variable with additional plugin search directories,
/// separated by the platform path separator.
pub use crate::config::SOURCE_PATH_ENV;

/// Scan plugin directories (in order) for `easymusic-source-*` executables.
/// Earlier directories win when two plugins share a name.
pub fn discover_external_sources(plugin_dirs: &[PathBuf]) -> Vec<PluginDescriptor> {
    let mut descriptors = Vec::new();
    let mut seen = Vec::new();
    for dir in plugin_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = file_name.strip_prefix(PLUGIN_PREFIX) else {
                continue;
            };
            let name = name.trim_end_matches(".exe").to_owned();
            if name.is_empty() || seen.contains(&name) || !is_executable(&entry.path()) {
                continue;
            }
            seen.push(name.clone());
            descriptors.push(PluginDescriptor {
                name,
                executable: entry.path(),
            });
        }
    }
    descriptors
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum PluginRequest<'a> {
    Search {
        protocol: &'static str,
        source: &'a str,
        keyword: &'a str,
        limit: usize,
    },
    Resolve {
        protocol: &'static str,
        source: &'a str,
        id: &'a str,
    },
}

#[derive(Debug, Deserialize)]
struct PluginEnvelope {
    ok: bool,
    error: Option<PluginError>,
    tracks: Option<Vec<PluginTrack>>,
    url: Option<String>,
    title: Option<String>,
    extension: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PluginTrack {
    id: String,
    title: String,
    artist: String,
    artwork_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PluginError {
    #[serde(default)]
    message: Option<String>,
}

/// `MusicSource` adapter that runs an external plugin executable.
pub struct ExternalSource {
    descriptor: PluginDescriptor,
    timeout: Duration,
}

impl ExternalSource {
    pub fn new(descriptor: PluginDescriptor) -> Self {
        Self::with_timeout(descriptor, PLUGIN_TIMEOUT)
    }

    /// Build a plugin source with a custom per-request deadline.
    pub fn with_timeout(descriptor: PluginDescriptor, timeout: Duration) -> Self {
        Self {
            descriptor,
            timeout,
        }
    }

    pub fn executable(&self) -> &Path {
        &self.descriptor.executable
    }

    async fn invoke(&self, request: &PluginRequest<'_>) -> Result<PluginEnvelope> {
        let payload = serde_json::to_vec(request)
            .map_err(|error| EasyMusicError::new(ErrorCode::Io, error.to_string()))?;
        let mut command = Command::new(&self.descriptor.executable);
        self.invoke_command(&mut command, payload).await
    }

    async fn invoke_command(
        &self,
        command: &mut Command,
        payload: Vec<u8>,
    ) -> Result<PluginEnvelope> {
        let output = crate::process::run(command, payload, self.timeout)
            .await
            .map_err(|error| {
                let code = match &error {
                    crate::process::ProcessError::Spawn(_) => ErrorCode::DependencyMissing,
                    crate::process::ProcessError::Setup(_)
                    | crate::process::ProcessError::Io(_) => ErrorCode::Io,
                    _ => ErrorCode::UpstreamApi,
                };
                EasyMusicError::new(code, format!("plugin {}: {error}", self.descriptor.name))
            })?;
        serde_json::from_slice(&output).map_err(|error| {
            EasyMusicError::upstream(format!(
                "plugin {} returned invalid JSON: {error}",
                self.descriptor.name
            ))
        })
    }

    fn failure(&self, envelope: &PluginEnvelope) -> EasyMusicError {
        EasyMusicError::upstream(format!(
            "plugin {} reported an error: {}",
            self.descriptor.name,
            envelope
                .error
                .as_ref()
                .and_then(|error| error.message.as_deref())
                .unwrap_or("unknown error")
        ))
    }
}

#[async_trait]
impl MusicSource for ExternalSource {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn display_name(&self) -> &str {
        self.name()
    }

    fn diagnostics(&self) -> SourceDiagnostics<'_> {
        SourceDiagnostics {
            executable: Some(&self.descriptor.executable),
            js_runtime: None,
        }
    }

    async fn search(&self, keyword: &str, limit: usize) -> Result<Vec<SourceTrack>> {
        let request = PluginRequest::Search {
            protocol: PROTOCOL_VERSION,
            source: &self.descriptor.name,
            keyword,
            limit,
        };
        let envelope = self.invoke(&request).await?;
        if !envelope.ok {
            return Err(self.failure(&envelope));
        }
        let tracks = envelope
            .tracks
            .unwrap_or_default()
            .into_iter()
            .filter(|track| !track.id.trim().is_empty() && !track.title.trim().is_empty())
            .map(|track| SourceTrack {
                // Legacy plugins may supply a qualified ID. Normalize only at
                // this protocol boundary; the registry never guesses ID contents.
                id: NativeTrackId::new(
                    track
                        .id
                        .trim()
                        .strip_prefix(&format!("{}:", self.name()))
                        .unwrap_or(track.id.trim()),
                ),
                title: track.title,
                artist: track.artist,
                artwork_url: track.artwork_url,
            })
            .collect::<Vec<_>>();
        Ok(tracks)
    }

    async fn resolve(&self, native: &NativeTrackId) -> Result<SourceResolvedTrack> {
        let id = native.as_str();
        let request = PluginRequest::Resolve {
            protocol: PROTOCOL_VERSION,
            source: &self.descriptor.name,
            id,
        };
        let envelope = self.invoke(&request).await?;
        if !envelope.ok {
            return Err(self.failure(&envelope));
        }
        let url = envelope
            .url
            .as_deref()
            .and_then(non_empty)
            .ok_or_else(|| self.failure(&envelope))?;
        validate_http_url(&url)?;
        Ok(SourceResolvedTrack {
            id: native.clone(),
            title: envelope
                .title
                .as_deref()
                .and_then(non_empty)
                .unwrap_or_else(|| id.to_owned()),
            url,
            extension: envelope.extension.as_deref().and_then(non_empty),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_names_are_extracted_from_file_names() {
        let dir =
            std::env::temp_dir().join(format!("easymusic-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("{PLUGIN_PREFIX}fake"));
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("not-a-plugin.txt"), b"x").unwrap();

        let descriptors = discover_external_sources(std::slice::from_ref(&dir));
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name, "fake");
        assert_eq!(descriptors[0].executable, fake);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn requests_use_the_documented_protocol_shape() {
        let request = PluginRequest::Search {
            protocol: PROTOCOL_VERSION,
            source: "fake",
            keyword: "晴天",
            limit: 5,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""protocol":"easymusic-source/1""#));
        assert!(json.contains(r#""operation":"search""#));
        assert!(json.contains(r#""keyword":"晴天""#));
    }

    #[test]
    fn envelopes_are_parsed_leniently() {
        let ok: PluginEnvelope = serde_json::from_str(
            r#"{"ok":true,"tracks":[{"id":"a1","title":"晴天","artist":"周杰伦"}]}"#,
        )
        .unwrap();
        assert!(ok.ok);
        assert_eq!(ok.tracks.unwrap().len(), 1);

        let failed: PluginEnvelope =
            serde_json::from_str(r#"{"ok":false,"error":{"message":"boom"}}"#).unwrap();
        assert!(!failed.ok);
        assert_eq!(failed.error.unwrap().message.unwrap(), "boom");
    }

    /// A plugin that answers normally must keep working with the concurrent
    /// writer, including plugins that read stdin to EOF.
    #[cfg(unix)]
    #[tokio::test]
    async fn well_behaved_plugin_round_trips() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("easymusic-roundtrip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join(format!("{PLUGIN_PREFIX}echo"));
        std::fs::write(
            &script,
            b"#!/bin/sh\ncat > /dev/null\nprintf '%s' '\
{\"ok\":true,\"tracks\":[{\"id\":\"e1\",\"title\":\"Qingtian\",\"artist\":\"Zhou Jielun\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let source = ExternalSource::new(PluginDescriptor {
            name: "echo".to_owned(),
            executable: script,
        });
        let result = source
            .search("晴天", 5)
            .await
            .expect("plugin should answer");
        assert_eq!(result.len(), 1);
        // Native IDs stay native here; the registry adds the namespace.
        assert_eq!(result[0].id.as_str(), "e1");
        assert_eq!(result[0].title, "Qingtian");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
