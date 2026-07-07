use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{
    RecordedAudio, TranscriptionClient, TranscriptionError, TranscriptionResult,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Errors returned by the transcriber library's public entry points.
///
/// The HTTP layer keeps its own `ApiError`; this type covers the non-HTTP
/// library functions (backend resolution, discovery-file I/O, and the limit
/// probe) so an app boundary can distinguish a recoverable auth/backend
/// failure from a fatal filesystem or serialization error without matching on
/// message text.
#[derive(Debug, thiserror::Error)]
pub enum TranscriberError {
    /// Resolving Codex authentication or constructing a transcription client
    /// failed.
    #[error(transparent)]
    Backend(#[from] TranscriptionError),
    /// A filesystem operation failed while probing limits or writing the
    /// discovery file. `context` preserves the operation description that the
    /// call site would previously have attached via `anyhow` context.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// Serializing the discovery document to JSON failed.
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    /// Splitting the source audio into chunks with ffmpeg failed during a
    /// limit probe.
    #[error("failed to split audio for limit probe: {0}")]
    Chunking(String),
    /// A discovery path was malformed (for example, it had no parent
    /// directory).
    #[error("{0}")]
    Discovery(String),
}

pub mod chunking;
pub mod client;
pub mod discovery;
pub mod server;
#[cfg(test)]
pub mod test_support;
pub mod upload;

pub use server::serve;

const DEFAULT_SERVICE_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub codex_upload_limit_bytes: u64,
    pub client_upload_limit_bytes: u64,
    pub chunk_seconds: u64,
    pub token_env: String,
    pub ffmpeg_binary: String,
    pub no_auth: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeLimitsConfig {
    pub file: PathBuf,
    pub codex_upload_limit_bytes: u64,
    pub chunk_seconds: u64,
    pub max_chunks: usize,
    pub include_oversized: bool,
    pub ffmpeg_binary: String,
}

pub struct ResolvedTranscriptionBackend {
    pub label: &'static str,
    pub client: RuntimeTranscriptionClient,
}

#[derive(Clone)]
pub enum RuntimeTranscriptionClient {
    Local {
        client: client::LocalTranscriberClient,
        fallback: Option<CodexTranscriptionClient>,
    },
    Direct(CodexTranscriptionClient),
}

#[async_trait::async_trait]
impl TranscriptionClient for RuntimeTranscriptionClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        match self {
            Self::Local { client, fallback } => match client.transcribe(recording).await {
                Ok(text) => Ok(text),
                // Client errors (4xx) from the local endpoint are not retryable;
                // server errors (5xx) indicate the local service is struggling
                // and direct Codex may still succeed.
                Err(error @ TranscriptionError::Service { status, .. }) if status < 500 => {
                    Err(error)
                }
                Err(error) => {
                    tracing::warn!(%error, "local transcriber failed, attempting direct Codex fallback");
                    if let Some(direct) = fallback {
                        direct.transcribe(recording).await
                    } else {
                        Err(error)
                    }
                }
            },
            Self::Direct(client) => client.transcribe(recording).await,
        }
    }
}

pub async fn resolve_transcription_backend(
) -> Result<ResolvedTranscriptionBackend, TranscriberError> {
    if let Some(local) =
        client::LocalTranscriberClient::discover(DEFAULT_PROBE_TIMEOUT, DEFAULT_RUNTIME_TIMEOUT)
            .await
    {
        let fallback = CodexAuthService::new()
            .and_then(|auth| CodexTranscriptionClient::with_timeout(auth, DEFAULT_RUNTIME_TIMEOUT))
            .map_err(|error| {
                tracing::warn!(%error, "failed to create direct fallback client; local-only mode");
            })
            .ok();
        return Ok(ResolvedTranscriptionBackend {
            label: "local-service",
            client: RuntimeTranscriptionClient::Local {
                client: local,
                fallback,
            },
        });
    }

    Ok(ResolvedTranscriptionBackend {
        label: "direct-codex",
        client: RuntimeTranscriptionClient::Direct(CodexTranscriptionClient::with_timeout(
            CodexAuthService::new()?,
            DEFAULT_RUNTIME_TIMEOUT,
        )?),
    })
}

pub async fn probe_limits(config: ProbeLimitsConfig) -> Result<(), TranscriberError> {
    let source_size = tokio::fs::metadata(&config.file)
        .await
        .map_err(|source| TranscriberError::Io {
            context: format!("failed to stat {}", config.file.display()),
            source,
        })?
        .len();
    println!("file: {}", config.file.display());
    println!("source_bytes: {source_size}");
    println!(
        "codex_upload_limit_bytes: {}",
        config.codex_upload_limit_bytes
    );

    let backend =
        CodexTranscriptionClient::with_timeout(CodexAuthService::new()?, DEFAULT_SERVICE_TIMEOUT)?;

    if source_size <= config.codex_upload_limit_bytes || config.include_oversized {
        probe_one(
            &backend,
            "source",
            &config.file,
            source_size,
            upload::source_content_type(&config.file),
        )
        .await;
    } else {
        println!("attempt=source status=skipped reason=exceeds_configured_limit");
    }

    if source_size <= config.codex_upload_limit_bytes {
        return Ok(());
    }

    if !chunking::ffmpeg_available(&config.ffmpeg_binary).await {
        println!("attempt=chunks status=skipped reason=ffmpeg_missing");
        return Ok(());
    }

    if config.max_chunks == 0 {
        println!("attempt=chunks status=skipped reason=max_chunks_zero");
        return Ok(());
    }

    let chunk_seconds =
        chunking::effective_chunk_seconds(config.chunk_seconds, config.codex_upload_limit_bytes);
    let chunks =
        chunking::split_audio_with_ffmpeg(&config.ffmpeg_binary, &config.file, chunk_seconds, None)
            .await
            .map_err(|error| TranscriberError::Chunking(error.to_string()))?;
    let limit = config.max_chunks.min(chunks.paths.len());
    for (index, path) in chunks.paths.iter().take(limit).enumerate() {
        let bytes = tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let label = format!("chunk-{index}");
        probe_one(&backend, &label, path, bytes, "audio/wav").await;
    }
    if chunks.paths.len() > limit {
        println!(
            "attempt=chunks status=partial tested={} total={}",
            limit,
            chunks.paths.len()
        );
    }
    Ok(())
}

async fn probe_one(
    backend: &CodexTranscriptionClient,
    label: &str,
    path: &std::path::Path,
    bytes: u64,
    content_type: &str,
) {
    let recording = RecordedAudio {
        path: path.to_path_buf(),
        content_type: content_type.to_string(),
        filename: upload::filename_for_path(path),
        // Duration is not consumed by the Codex transcription endpoint for probe requests.
        duration: Duration::default(),
    };
    match backend.transcribe(&recording).await {
        Ok(transcript) => {
            println!(
                "attempt={label} bytes={bytes} status=ok transcript_chars={}",
                transcript.chars().count()
            );
        }
        Err(error) => {
            let redacted = codex_voice_core::redact_diagnostics(&error.to_string());
            let truncated = if redacted.len() > 1500 {
                let mut t = redacted;
                t.truncate(1500);
                t.push_str("...");
                t
            } else {
                redacted
            };
            println!("attempt={label} bytes={bytes} status=error error={truncated}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_display_includes_context_and_source() {
        let error = TranscriberError::Io {
            context: "failed to stat /tmp/recording.wav".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        };
        assert_eq!(
            error.to_string(),
            "failed to stat /tmp/recording.wav: no such file"
        );
    }

    #[test]
    fn chunking_error_display_preserves_probe_context() {
        let error = TranscriberError::Chunking("ffmpeg failed with status 1".to_string());
        assert_eq!(
            error.to_string(),
            "failed to split audio for limit probe: ffmpeg failed with status 1"
        );
    }
}
