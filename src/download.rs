use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{Client, Response, Url};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::AsyncWriteExt;

use crate::error::{EasyMusicError, ErrorCode, Result};
use crate::model::{DownloadConfig, DownloadedFile};
use crate::network::reject_private_host;
use crate::network::validate_http_url;

const MAX_REDIRECTS: usize = 10;
const TEMP_FILE_ATTEMPTS: usize = 100;

/// Download a remote audio file without transcoding it.
///
/// Bytes are written to a temporary sibling file and moved into place only
/// after the response completes, so interrupted downloads do not leave a
/// partial destination file.
pub async fn download_audio(config: &DownloadConfig) -> Result<DownloadedFile> {
    prepare_destination(&config.output, config.overwrite).await?;

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("easymusic/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| EasyMusicError::source(error.to_string()))?;
    let initial_url = validate_http_url(&config.url)?;
    let (mut response, final_url) =
        fetch_response(&client, initial_url, config.allow_private_network).await?;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let (mut file, temporary_path) = create_temporary_file(&config.output).await?;
    let write_result = async {
        let mut bytes_written = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| EasyMusicError::source(error.to_string()))?
        {
            file.write_all(&chunk).await?;
            bytes_written += chunk.len() as u64;
        }
        if bytes_written == 0 {
            return Err(EasyMusicError::source(
                "audio server returned an empty response",
            ));
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        install_download(&temporary_path, &config.output, config.overwrite).await?;
        Ok(bytes_written)
    }
    .await;

    let bytes_written = match write_result {
        Ok(bytes_written) => bytes_written,
        Err(error) => {
            let _ = fs::remove_file(&temporary_path).await;
            return Err(error);
        }
    };
    let path = fs::canonicalize(&config.output)
        .await
        .unwrap_or_else(|_| config.output.clone());
    let path = normalize_canonical_path(path);

    Ok(DownloadedFile {
        path,
        source_url: final_url.into(),
        bytes_written,
        content_type,
    })
}

#[cfg(windows)]
fn normalize_canonical_path(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(value) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{value}"));
    }
    if let Some(value) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(value);
    }
    path
}

#[cfg(not(windows))]
fn normalize_canonical_path(path: PathBuf) -> PathBuf {
    path
}

async fn fetch_response(
    client: &Client,
    mut url: Url,
    allow_private_network: bool,
) -> Result<(Response, Url)> {
    for redirect_count in 0..=MAX_REDIRECTS {
        if !allow_private_network {
            reject_private_host(&url).await?;
        }
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| EasyMusicError::source(error.to_string()))?;

        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(EasyMusicError::source(
                    "audio download exceeded 10 redirects",
                ));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .ok_or_else(|| EasyMusicError::source("redirect response has no Location header"))?
                .to_str()
                .map_err(|_| EasyMusicError::source("redirect Location is not valid UTF-8"))?;
            url = url
                .join(location)
                .map_err(|error| EasyMusicError::source(error.to_string()))?;
            validate_http_url(url.as_str())?;
            continue;
        }

        let response = response
            .error_for_status()
            .map_err(|error| EasyMusicError::source(error.to_string()))?;
        return Ok((response, url));
    }
    unreachable!("redirect loop always returns or continues")
}

async fn prepare_destination(output: &Path, overwrite: bool) -> Result<()> {
    if output.file_name().is_none() {
        return Err(EasyMusicError::invalid(
            "download output must be a file path",
        ));
    }
    match fs::metadata(output).await {
        Ok(metadata) if metadata.is_dir() => {
            return Err(EasyMusicError::invalid(
                "download output points to a directory",
            ));
        }
        Ok(_) if !overwrite => {
            return Err(EasyMusicError::new(
                ErrorCode::Io,
                format!(
                    "download output already exists: {}; use --force to overwrite",
                    output.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).await?;
    }
    Ok(())
}

async fn create_temporary_file(output: &Path) -> Result<(File, PathBuf)> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download");

    for attempt in 0..TEMP_FILE_ATTEMPTS {
        let temporary_path = parent.join(format!(
            ".{file_name}.easymusic-{}-{attempt}.part",
            std::process::id()
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .await
        {
            Ok(file) => return Ok((file, temporary_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(EasyMusicError::new(
        ErrorCode::Io,
        "could not reserve a temporary download file",
    ))
}

async fn install_download(temporary: &Path, output: &Path, overwrite: bool) -> Result<()> {
    if overwrite {
        match fs::remove_file(output).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::rename(temporary, output).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn downloads_to_an_atomic_local_file() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc123",
                )
                .await
                .unwrap();
        });
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output = std::env::temp_dir().join(format!("easymusic-{unique}.mp3"));
        let result = download_audio(&DownloadConfig {
            url: format!("http://{address}/song.mp3"),
            output: output.clone(),
            overwrite: false,
            allow_private_network: true,
        })
        .await
        .unwrap();

        server.await.unwrap();
        assert_eq!(result.bytes_written, 6);
        assert_eq!(result.content_type.as_deref(), Some("audio/mpeg"));
        assert_eq!(fs::read(&output).await.unwrap(), b"abc123");
        fs::remove_file(output).await.unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn removes_windows_extended_path_prefixes() {
        assert_eq!(
            normalize_canonical_path(PathBuf::from(r"\\?\C:\music\song.mp3")),
            PathBuf::from(r"C:\music\song.mp3")
        );
        assert_eq!(
            normalize_canonical_path(PathBuf::from(r"\\?\UNC\server\music\song.mp3")),
            PathBuf::from(r"\\server\music\song.mp3")
        );
    }
}
