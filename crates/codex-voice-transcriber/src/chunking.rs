use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{process::Command as TokioCommand, time};

#[derive(Debug, Clone)]
pub enum ChunkingError {
    TooManyChunks {
        count: usize,
        limit: usize,
    },
    ChunkTooLarge {
        index: usize,
        bytes: u64,
        limit: u64,
    },
    DecodedTooLarge {
        bytes: u64,
        limit: u64,
    },
    Io {
        message: String,
    },
}

pub const MAX_GENERATED_CHUNKS: usize = 512;
pub const PCM_BYTES_PER_SECOND: u64 = 16_000_u64 * 2;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub struct ChunkedAudio {
    pub(super) _dir: TempDir,
    pub(super) paths: Vec<PathBuf>,
}

pub async fn ffmpeg_available(binary: &str) -> bool {
    let mut command = TokioCommand::new(binary);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    match time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            false
        }
    }
}

pub fn ffprobe_binary(ffmpeg_binary: &str) -> String {
    let path = Path::new(ffmpeg_binary);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ffmpeg");
    // Replace only the exact filename stem, not arbitrary substrings.
    let ffprobe_name = if let Some(stem) = Path::new(file_name).file_stem().and_then(|s| s.to_str())
    {
        if stem == "ffmpeg" || stem == "ffmpeg.exe" {
            file_name.replacen("ffmpeg", "ffprobe", 1)
        } else {
            file_name.to_string()
        }
    } else {
        file_name.to_string()
    };
    path.with_file_name(&ffprobe_name)
        .to_string_lossy()
        .into_owned()
}

pub async fn input_duration_seconds(binary: &str, input: &Path) -> Result<Option<f64>> {
    let mut command = TokioCommand::new(binary);
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {binary}"))?;
    let status = match time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => anyhow::bail!("failed to wait for ffprobe: {error}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("ffprobe timed out after 5s");
        }
    };
    if !status.success() {
        return Ok(None);
    }
    let stdout = child.stdout.take().context("ffprobe stdout unavailable")?;
    let mut buffer = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::BufReader::new(stdout), &mut buffer)
        .await
        .context("failed to read ffprobe output")?;
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<f64>()
        .map(Some)
        .context("ffprobe output is not a valid duration")
}

pub async fn split_audio_with_ffmpeg(
    binary: &str,
    input: &Path,
    chunk_seconds: u64,
    max_duration_seconds: Option<u64>,
) -> Result<ChunkedAudio> {
    let dir =
        tokio::task::spawn_blocking(|| TempDir::new().context("failed to create chunk temp dir"))
            .await
            .context("chunk temp dir task panicked")??;
    let dir_path = dir.path().to_path_buf();
    let pattern = dir_path.join("chunk-%04d.wav");
    let segment_time = chunk_seconds.max(1).to_string();
    let duration_limit = max_duration_seconds.map(|seconds| seconds.max(1).to_string());

    let mut command = TokioCommand::new(binary);
    command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input);
    if let Some(duration_limit) = duration_limit.as_deref() {
        command.args(["-t", duration_limit]);
    }
    command
        .args([
            "-vn",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            "-f",
            "segment",
            "-segment_time",
            &segment_time,
            "-reset_timestamps",
            "1",
        ])
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {binary}"))?;
    let status = match time::timeout(FFMPEG_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => anyhow::bail!("failed to wait for ffmpeg: {error}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("ffmpeg timed out after {}s", FFMPEG_TIMEOUT.as_secs());
        }
    };
    if !status.success() {
        anyhow::bail!("ffmpeg failed with status {status}");
    }
    let mut entries = tokio::fs::read_dir(&dir_path)
        .await
        .context("failed to list generated chunks")?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.transpose() {
        if let Ok(entry) = entry {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("wav") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    if paths.is_empty() {
        anyhow::bail!("ffmpeg did not produce any audio chunks");
    }
    Ok(ChunkedAudio { _dir: dir, paths })
}

pub async fn validate_generated_chunks(
    paths: &[PathBuf],
    max_decoded_bytes: u64,
    codex_upload_limit_bytes: u64,
) -> Result<(), ChunkingError> {
    if paths.len() > MAX_GENERATED_CHUNKS {
        return Err(ChunkingError::TooManyChunks {
            count: paths.len(),
            limit: MAX_GENERATED_CHUNKS,
        });
    }

    let mut total_bytes = 0_u64;
    for (index, path) in paths.iter().enumerate() {
        let bytes = tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len())
            .map_err(|error| ChunkingError::Io {
                message: format!("failed to stat generated chunk {index}: {error}"),
            })?;
        if bytes > codex_upload_limit_bytes {
            return Err(ChunkingError::ChunkTooLarge {
                index,
                bytes,
                limit: codex_upload_limit_bytes,
            });
        }
        total_bytes = total_bytes.saturating_add(bytes);
        if total_bytes > max_decoded_bytes {
            return Err(ChunkingError::DecodedTooLarge {
                bytes: total_bytes,
                limit: max_decoded_bytes,
            });
        }
    }
    Ok(())
}

pub fn effective_chunk_seconds(requested_seconds: u64, upload_limit_bytes: u64) -> u64 {
    let reserve = 1_024 * 1_024;
    let usable = upload_limit_bytes
        .saturating_sub(reserve)
        .max(PCM_BYTES_PER_SECOND);
    let max_seconds = (usable / PCM_BYTES_PER_SECOND).max(1);
    requested_seconds.max(1).min(max_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_seconds_respect_upload_limit() {
        assert_eq!(effective_chunk_seconds(600, 24 * 1024 * 1024), 600);
        assert!(effective_chunk_seconds(600, 2 * 1024 * 1024) < 600);
        assert_eq!(effective_chunk_seconds(0, 24 * 1024 * 1024), 1);
    }

    #[tokio::test]
    async fn generated_chunk_limits_reject_decoded_growth() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = dir.path().join("chunk-0000.wav");
        let second = dir.path().join("chunk-0001.wav");
        std::fs::write(&first, [0_u8; 8]).expect("first chunk");
        std::fs::write(&second, [0_u8; 8]).expect("second chunk");

        let error = validate_generated_chunks(&[first, second], 12, 16)
            .await
            .expect_err("limit rejects");

        assert!(matches!(error, ChunkingError::DecodedTooLarge { .. }));
    }

    #[tokio::test]
    async fn generated_chunk_limits_reject_many_chunks() {
        let paths = (0..=MAX_GENERATED_CHUNKS)
            .map(|index| PathBuf::from(format!("chunk-{index:04}.wav")))
            .collect::<Vec<_>>();

        let error = validate_generated_chunks(&paths, 1024, 1024)
            .await
            .expect_err("chunk count rejects");

        assert!(matches!(error, ChunkingError::TooManyChunks { .. }));
    }
}
