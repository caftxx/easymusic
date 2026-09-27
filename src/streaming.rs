use std::process::Stdio;

use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{EasyMusicError, ErrorCode, Result};
use crate::model::{AudioChunk, AudioChunkKind, AudioFormat, StreamConfig, StreamStats};
use crate::network::reject_private_host;
use crate::network::validate_http_url;

const MAX_OGG_PACKET_BYTES: usize = 16 * 1024 * 1024;
const AUDIO_CHANNEL_CAPACITY: usize = 8;
const BYTE_CHUNK_SIZE: usize = 16 * 1024;
const NETWORK_RW_TIMEOUT_MICROS: &str = "15000000";
const HTTP_RECONNECT_DELAY_MAX_SECONDS: &str = "2";

pub struct AudioStream {
    receiver: mpsc::Receiver<AudioChunk>,
    task: Option<JoinHandle<Result<StreamStats>>>,
}

impl AudioStream {
    /// Receive the next byte chunk or raw Opus packet.
    ///
    /// `None` means the producer stopped. Call [`Self::finish`] afterward to
    /// distinguish successful EOF from an ffmpeg or decoding error.
    pub async fn next_chunk(&mut self) -> Option<AudioChunk> {
        self.receiver.recv().await
    }

    /// Wait for ffmpeg to exit and return producer-side statistics.
    pub async fn finish(mut self) -> Result<StreamStats> {
        let task = self
            .task
            .take()
            .ok_or_else(|| EasyMusicError::transcode("audio stream task is unavailable"))?;
        task.await.map_err(|error| {
            EasyMusicError::transcode(format!("audio stream task failed: {error}"))
        })?
    }

    #[cfg(test)]
    pub(crate) fn from_test_chunks(chunks: Vec<AudioChunk>) -> Self {
        let (sender, receiver) = mpsc::channel(chunks.len().max(1));
        let task = tokio::spawn(async move {
            let mut stats = StreamStats::default();
            for chunk in chunks {
                stats.bytes_written += chunk.data.len() as u64;
                if chunk.kind == AudioChunkKind::OpusPacket {
                    stats.packets_written += 1;
                }
                sender.send(chunk).await.map_err(|_| {
                    EasyMusicError::new(ErrorCode::Interrupted, "test consumer closed")
                })?;
            }
            Ok(stats)
        });
        Self {
            receiver,
            task: Some(task),
        }
    }
}

impl Drop for AudioStream {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            // The task owns a kill_on_drop ffmpeg child. Aborting the consumer
            // therefore terminates transcoding instead of leaking a process.
            task.abort();
        }
    }
}

/// Start ffmpeg and expose a bounded, backpressured async chunk stream.
///
/// PCM and Ogg output use [`AudioChunkKind::Bytes`]. `opus-packets` strips the
/// Ogg container and yields one [`AudioChunkKind::OpusPacket`] per item, ready
/// for a device WebSocket binary message.
pub async fn spawn_audio_stream(config: &StreamConfig) -> Result<AudioStream> {
    let url = validate_http_url(&config.url)?;
    if !config.allow_private_network {
        reject_private_host(&url).await?;
    }
    validate_stream_config(config)?;

    let mut command = build_ffmpeg_command(config);
    let mut child = command.spawn().map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            ErrorCode::DependencyMissing
        } else {
            ErrorCode::Transcode
        };
        EasyMusicError::new(code, format!("failed to start ffmpeg: {error}"))
    })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| EasyMusicError::transcode("ffmpeg stdout was not captured"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| EasyMusicError::transcode("ffmpeg stderr was not captured"))?;
    let format = config.format;
    let (sender, receiver) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);

    let task = tokio::spawn(async move {
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            bytes
        });

        let stats = match format {
            AudioFormat::OpusPackets => read_opus_packets(&mut stdout, &sender).await?,
            AudioFormat::PcmS16le | AudioFormat::OpusOgg => {
                read_byte_chunks(&mut stdout, &sender).await?
            }
        };
        drop(sender);

        let status = child.wait().await?;
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        if !status.success() {
            let message = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
            return Err(EasyMusicError::transcode(if message.is_empty() {
                format!("ffmpeg exited with {status}")
            } else {
                format!("ffmpeg exited with {status}: {message}")
            }));
        }
        Ok(stats)
    });

    Ok(AudioStream {
        receiver,
        task: Some(task),
    })
}

/// Stream to the CLI-selected file or stdout.
///
/// Library integrations that need frame boundaries should use
/// [`spawn_audio_stream`] instead.
pub async fn stream_audio(config: &StreamConfig) -> Result<StreamStats> {
    let mut stream = spawn_audio_stream(config).await?;

    if config.events_json {
        eprintln!(
            "{}",
            json!({
                "type": "stream.started",
                "format": config.format,
                "sample_rate": config.sample_rate,
                "channels": config.channels,
                "bitrate": config.bitrate,
                "frame_ms": config.frame_ms,
            })
        );
    }

    let mut output: Box<dyn AsyncWrite + Unpin + Send> = match &config.output {
        Some(path) => Box::new(tokio::fs::File::create(path).await?),
        None => Box::new(tokio::io::stdout()),
    };
    let mut stats = StreamStats::default();

    loop {
        let chunk = tokio::select! {
            chunk = stream.next_chunk() => chunk,
            signal_result = tokio::signal::ctrl_c() => {
                let _ = signal_result;
                return Err(EasyMusicError::new(ErrorCode::Interrupted, "stream interrupted"));
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.kind == AudioChunkKind::OpusPacket {
            let packet_len = u32::try_from(chunk.data.len())
                .map_err(|_| EasyMusicError::transcode("Opus packet is too large"))?;
            output.write_all(&packet_len.to_be_bytes()).await?;
            stats.bytes_written += 4;
            stats.packets_written += 1;
        }
        output.write_all(&chunk.data).await?;
        stats.bytes_written += chunk.data.len() as u64;
    }
    output.flush().await?;
    stream.finish().await?;

    if config.events_json {
        eprintln!(
            "{}",
            json!({
                "type": "stream.completed",
                "bytes_written": stats.bytes_written,
                "packets_written": stats.packets_written,
            })
        );
    }
    Ok(stats)
}

fn build_ffmpeg_command(config: &StreamConfig) -> Command {
    let mut command = Command::new(&config.ffmpeg);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"]);

    if let Some(start) = config.start_seconds {
        command.args(["-ss", &format!("{start:.3}")]);
    }
    command.args([
        "-protocol_whitelist",
        "http,https,tcp,tls,crypto",
        "-rw_timeout",
        NETWORK_RW_TIMEOUT_MICROS,
        "-reconnect",
        "1",
        "-reconnect_streamed",
        "1",
        "-reconnect_delay_max",
        HTTP_RECONNECT_DELAY_MAX_SECONDS,
        "-user_agent",
        concat!("easymusic/", env!("CARGO_PKG_VERSION")),
        "-i",
        &config.url,
        "-map",
        "0:a:0",
        "-vn",
        "-sn",
        "-dn",
        "-ac",
        &config.channels.to_string(),
        "-ar",
        &config.sample_rate.to_string(),
    ]);
    if let Some(duration) = config.duration_seconds {
        command.args(["-t", &format!("{duration:.3}")]);
    }

    match config.format {
        AudioFormat::PcmS16le => {
            command.args(["-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"]);
        }
        AudioFormat::OpusOgg | AudioFormat::OpusPackets => {
            command.args([
                "-c:a",
                "libopus",
                "-b:a",
                &config.bitrate.to_string(),
                "-vbr",
                "on",
                "-application",
                "audio",
                "-frame_duration",
                &format_frame_duration(config.frame_ms),
                "-f",
                "opus",
                "pipe:1",
            ]);
        }
    }
    command
}

fn validate_stream_config(config: &StreamConfig) -> Result<()> {
    if config.channels == 0 || config.channels > 2 {
        return Err(EasyMusicError::invalid("channels must be 1 or 2"));
    }
    if !(8_000..=192_000).contains(&config.sample_rate) {
        return Err(EasyMusicError::invalid(
            "sample rate must be between 8000 and 192000",
        ));
    }
    if config.bitrate < 6_000 || config.bitrate > 512_000 {
        return Err(EasyMusicError::invalid(
            "bitrate must be between 6000 and 512000 bits/s",
        ));
    }
    if matches!(
        config.format,
        AudioFormat::OpusOgg | AudioFormat::OpusPackets
    ) && ![2.5_f32, 5.0, 10.0, 20.0, 40.0, 60.0]
        .iter()
        .any(|allowed| (config.frame_ms - allowed).abs() < f32::EPSILON)
    {
        return Err(EasyMusicError::invalid(
            "Opus frame-ms must be one of 2.5, 5, 10, 20, 40, or 60",
        ));
    }
    if config.start_seconds.is_some_and(|value| value < 0.0)
        || config.duration_seconds.is_some_and(|value| value <= 0.0)
    {
        return Err(EasyMusicError::invalid(
            "start-seconds must be >= 0 and duration-seconds must be > 0",
        ));
    }
    Ok(())
}

fn format_frame_duration(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as u32)
    } else {
        format!("{value:.1}")
    }
}

async fn read_byte_chunks<R>(
    reader: &mut R,
    sender: &mpsc::Sender<AudioChunk>,
) -> Result<StreamStats>
where
    R: AsyncRead + Unpin,
{
    let mut stats = StreamStats::default();
    loop {
        let mut buffer = vec![0_u8; BYTE_CHUNK_SIZE];
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(stats);
        }
        buffer.truncate(count);
        stats.bytes_written += count as u64;
        sender
            .send(AudioChunk {
                kind: AudioChunkKind::Bytes,
                data: buffer,
            })
            .await
            .map_err(|_| EasyMusicError::new(ErrorCode::Interrupted, "audio consumer closed"))?;
    }
}

async fn read_opus_packets<R>(
    reader: &mut R,
    sender: &mpsc::Sender<AudioChunk>,
) -> Result<StreamStats>
where
    R: AsyncRead + Unpin,
{
    let mut packet = Vec::new();
    let mut packet_index = 0_u64;
    let mut stats = StreamStats::default();

    loop {
        let mut header = [0_u8; 27];
        match reader.read_exact(&mut header[..1]).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }
        reader.read_exact(&mut header[1..]).await?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Err(EasyMusicError::transcode(
                "ffmpeg produced an invalid Ogg Opus stream",
            ));
        }

        let segment_count = header[26] as usize;
        let mut lacing = vec![0_u8; segment_count];
        reader.read_exact(&mut lacing).await?;
        let payload_len: usize = lacing.iter().map(|value| *value as usize).sum();
        let mut payload = vec![0_u8; payload_len];
        reader.read_exact(&mut payload).await?;

        let mut offset = 0;
        for segment_len in lacing {
            let segment_len = segment_len as usize;
            packet.extend_from_slice(&payload[offset..offset + segment_len]);
            offset += segment_len;
            if packet.len() > MAX_OGG_PACKET_BYTES {
                return Err(EasyMusicError::transcode(
                    "Ogg packet is unreasonably large",
                ));
            }
            if segment_len < 255 {
                packet_index += 1;
                // Ogg Opus begins with OpusHead and OpusTags packets.
                if packet_index > 2 && !packet.is_empty() {
                    stats.bytes_written += packet.len() as u64;
                    stats.packets_written += 1;
                    sender
                        .send(AudioChunk {
                            kind: AudioChunkKind::OpusPacket,
                            data: std::mem::take(&mut packet),
                        })
                        .await
                        .map_err(|_| {
                            EasyMusicError::new(ErrorCode::Interrupted, "audio consumer closed")
                        })?;
                }
                packet.clear();
            }
        }
    }

    if !packet.is_empty() {
        return Err(EasyMusicError::transcode(
            "truncated Ogg stream ended inside an Opus packet",
        ));
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stream_config() -> StreamConfig {
        StreamConfig {
            url: "https://example.com/song.mp3".to_owned(),
            format: AudioFormat::OpusPackets,
            sample_rate: 24_000,
            channels: 1,
            bitrate: 64_000,
            frame_ms: 60.0,
            framing: crate::model::Framing::Len32be,
            output: None,
            ffmpeg: PathBuf::from("ffmpeg"),
            allow_private_network: false,
            start_seconds: None,
            duration_seconds: None,
            events_json: false,
        }
    }

    fn ogg_page(packets: &[&[u8]]) -> Vec<u8> {
        let lacing: Vec<u8> = packets
            .iter()
            .map(|packet| u8::try_from(packet.len()).unwrap())
            .collect();
        let mut page = vec![0_u8; 27];
        page[..4].copy_from_slice(b"OggS");
        page[4] = 0;
        page[26] = lacing.len() as u8;
        page.extend_from_slice(&lacing);
        for packet in packets {
            page.extend_from_slice(packet);
        }
        page
    }

    #[tokio::test]
    async fn strips_ogg_headers_and_frames_raw_opus_packets() {
        let input = [
            ogg_page(&[b"OpusHead", b"OpusTags"]),
            ogg_page(&[b"first", b"second"]),
        ]
        .concat();
        let mut reader = std::io::Cursor::new(input);
        let (sender, mut receiver) = mpsc::channel(4);
        let stats = read_opus_packets(&mut reader, &sender).await.unwrap();
        drop(sender);
        let mut packets = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            assert_eq!(chunk.kind, AudioChunkKind::OpusPacket);
            packets.push(chunk.data);
        }

        assert_eq!(stats.packets_written, 2);
        assert_eq!(packets, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn ffmpeg_http_input_has_reconnect_and_io_timeout_options() {
        let command = build_ffmpeg_command(&stream_config());
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let input_index = args.iter().position(|arg| arg == "-i").unwrap();
        let input_args = &args[..input_index];
        let option_value = |name: &str| {
            let index = input_args.iter().position(|arg| arg == name).unwrap();
            input_args[index + 1].as_str()
        };

        assert_eq!(option_value("-rw_timeout"), NETWORK_RW_TIMEOUT_MICROS);
        assert_eq!(option_value("-reconnect"), "1");
        assert_eq!(option_value("-reconnect_streamed"), "1");
        assert_eq!(
            option_value("-reconnect_delay_max"),
            HTTP_RECONNECT_DELAY_MAX_SECONDS
        );
    }
}
