use super::{authorize, ApiError, ServiceState};

use axum::{
    extract::{FromRequest, Multipart, Request, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use crate::chunking::{self, MAX_GENERATED_CHUNKS, PCM_BYTES_PER_SECOND};
use crate::client;
use crate::upload::{self, Upload};
use codex_voice_core::TranscriptionClient;

/// Upstream transcription requests in flight per chunked upload.
const CHUNK_TRANSCRIBE_CONCURRENCY: usize = 4;

pub(crate) async fn transcribe(
    State(state): State<ServiceState>,
    request: Request,
) -> Result<Response, ApiError> {
    authorize(request.headers(), &state.auth)?;
    let multipart = Multipart::from_request(request, &state)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("length limit") || message.contains("Payload Too Large") {
                ApiError::payload_too_large(format!("request body exceeds size limit: {message}"))
            } else {
                ApiError::bad_request(format!("failed to read multipart form: {message}"))
            }
        })?;
    let upload = upload::read_upload(multipart, state.client_upload_limit_bytes).await?;
    let text = transcribe_upload(&state, &upload).await?;
    Ok(match upload.response_format {
        upload::ResponseFormat::Json => Json(TranscriptionResponse { text }).into_response(),
        upload::ResponseFormat::Text => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
    })
}

async fn transcribe_upload(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    if upload.bytes <= state.codex_upload_limit_bytes {
        return transcribe_direct(state, upload).await;
    }

    transcribe_chunked(state, upload).await
}

async fn transcribe_direct(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    client::transcribe_path(
        state.backend.as_ref(),
        upload.temp.path(),
        &upload.filename,
        &upload.content_type,
    )
    .await
    .map_err(|error| ApiError::backend(error.to_string()))
}

async fn transcribe_chunked(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    if !chunking::ffmpeg_available(&state.ffmpeg_binary).await {
        return Err(ApiError::payload_too_large(format!(
            "audio is {} bytes, above the Codex per-request limit of {} bytes; install ffmpeg or send smaller chunks",
            upload.bytes, state.codex_upload_limit_bytes
        )));
    }

    let chunk_seconds =
        chunking::effective_chunk_seconds(state.chunk_seconds, state.codex_upload_limit_bytes);
    let max_seconds_from_bytes = state.client_upload_limit_bytes / PCM_BYTES_PER_SECOND;
    let max_seconds_from_chunks = MAX_GENERATED_CHUNKS as u64 * chunk_seconds;
    let max_duration_seconds = max_seconds_from_bytes.min(max_seconds_from_chunks).max(1);

    match chunking::input_duration_seconds(
        &chunking::ffprobe_binary(&state.ffmpeg_binary),
        upload.temp.path(),
    )
    .await
    {
        Ok(Some(duration)) if duration > max_duration_seconds as f64 => {
            return Err(ApiError::payload_too_large(format!(
                "audio duration is {duration:.1}s, above the service limit of {max_duration_seconds}s; send smaller chunks"
            )));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to probe audio duration, proceeding with chunk-count safety cap");
        }
    }

    let chunks = chunking::split_audio_with_ffmpeg(
        &state.ffmpeg_binary,
        upload.temp.path(),
        chunk_seconds,
        Some(max_duration_seconds),
    )
    .await
    .map_err(|error| ApiError::internal(format!("failed to split oversized audio: {error:#}")))?;
    chunking::validate_generated_chunks(
        &chunks.paths,
        state.client_upload_limit_bytes,
        state.codex_upload_limit_bytes,
    )
    .await
    .map_err(|error| match error {
        chunking::ChunkingError::TooManyChunks { count, limit } => ApiError::payload_too_large(
            format!(
                "audio produced {count} chunks, above the service limit of {limit}; send smaller chunks"
            ),
        ),
        chunking::ChunkingError::ChunkTooLarge { index, bytes, limit } => {
            ApiError::payload_too_large(format!(
                "generated chunk {index} is {bytes} bytes, above configured Codex limit of {limit} bytes"
            ))
        }
        chunking::ChunkingError::DecodedTooLarge { bytes, limit } => ApiError::payload_too_large(
            format!(
                "decoded audio is {bytes} bytes, above the service decoded-output limit of {limit} bytes; send smaller chunks"
            ),
        ),
        chunking::ChunkingError::Io { message } => ApiError::internal(message),
    })?;
    transcribe_chunk_paths(&state.backend, &chunks.paths).await
}

/// Transcribes each chunk path with bounded concurrency, preserving chunk
/// order in the joined result (required by `upload::join_transcripts`).
///
/// Uses `buffered` (not `buffer_unordered`): it runs up to
/// `CHUNK_TRANSCRIBE_CONCURRENCY` requests concurrently while yielding
/// results in the original input order, so no reordering step is needed
/// before joining. Fails fast on the first error, matching the previous
/// serial loop's `?`-propagation semantics.
pub(super) async fn transcribe_chunk_paths(
    backend: &Arc<dyn TranscriptionClient>,
    paths: &[PathBuf],
) -> Result<String, ApiError> {
    let transcripts: Vec<String> = stream::iter(paths.to_vec())
        .map(|path| {
            let filename = upload::filename_for_path(&path);
            let backend = Arc::clone(backend);
            async move {
                client::transcribe_path(backend.as_ref(), &path, &filename, "audio/wav")
                    .await
                    .map_err(|error| ApiError::backend(error.to_string()))
            }
        })
        .buffered(CHUNK_TRANSCRIBE_CONCURRENCY)
        .try_collect()
        .await?;
    Ok(upload::join_transcripts(&transcripts))
}

#[derive(Debug, Serialize)]
struct TranscriptionResponse {
    text: String,
}
