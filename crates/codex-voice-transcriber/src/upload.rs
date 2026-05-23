use anyhow::Result;
use axum::extract::Multipart;
use codex_voice_core::TranscriptionError;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

use super::server::ApiError;

pub struct Upload {
    pub(super) temp: NamedTempFile,
    pub(super) filename: String,
    pub(super) content_type: String,
    pub(super) bytes: u64,
    pub(super) response_format: ResponseFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Json,
    Text,
}

pub async fn read_upload(
    mut multipart: Multipart,
    client_upload_limit_bytes: u64,
) -> Result<Upload, ApiError> {
    let mut upload = None;
    let mut response_format = ResponseFormat::Json;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("failed to read multipart form: {error}")))?
    {
        match field.name() {
            Some("file") => {
                if upload.is_some() {
                    return Err(ApiError::bad_request(
                        "multipart form included more than one file field",
                    ));
                }
                upload = Some(read_file_field(field, client_upload_limit_bytes).await?);
            }
            Some("response_format") => {
                response_format = read_response_format_field(&mut field).await?;
            }
            _ => {}
        }
    }

    let mut upload = upload
        .ok_or_else(|| ApiError::bad_request("multipart form did not include a file field"))?;
    upload.response_format = response_format;
    Ok(upload)
}

async fn read_file_field(
    mut field: axum::extract::multipart::Field<'_>,
    client_upload_limit_bytes: u64,
) -> Result<Upload, ApiError> {
    let filename = field
        .file_name()
        .map(sanitize_filename)
        .unwrap_or_else(|| "audio.wav".to_string());
    let content_type = field
        .content_type()
        .map(ToString::to_string)
        .unwrap_or_else(|| source_content_type(Path::new(&filename)).to_string());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(4);
    let write_task = tokio::task::spawn_blocking(move || {
        let mut temp = NamedTempFile::new().map_err(|error| {
            ApiError::internal(format!("failed to create temp upload: {error}"))
        })?;
        while let Some(chunk) = rx.blocking_recv() {
            temp.write_all(&chunk).map_err(|error| {
                ApiError::internal(format!("failed to write temp upload: {error}"))
            })?;
        }
        Ok::<_, ApiError>(temp)
    });
    let mut bytes = 0_u64;
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        let message = error.to_string();
        if message.contains("length limit") || message.contains("Payload Too Large") {
            ApiError::payload_too_large(format!("failed to read upload chunk: {message}"))
        } else {
            ApiError::bad_request(format!("failed to read upload chunk: {message}"))
        }
    })? {
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > client_upload_limit_bytes {
            drop(tx);
            let _ = write_task.await;
            return Err(ApiError::payload_too_large(format!(
                "upload exceeds client limit of {client_upload_limit_bytes} bytes"
            )));
        }
        tx.send(chunk)
            .await
            .map_err(|error| ApiError::internal(format!("temp write channel closed: {error}")))?;
    }
    drop(tx);
    let temp = write_task
        .await
        .map_err(|error| ApiError::internal(format!("temp write task panicked: {error}")))??;
    Ok(Upload {
        temp,
        filename,
        content_type,
        bytes,
        response_format: ResponseFormat::Json,
    })
}

async fn read_response_format_field(
    field: &mut axum::extract::multipart::Field<'_>,
) -> Result<ResponseFormat, ApiError> {
    const MAX_RESPONSE_FORMAT_BYTES: usize = 64;

    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        ApiError::bad_request(format!("failed to read response_format field: {error}"))
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_FORMAT_BYTES {
            return Err(ApiError::bad_request("response_format field is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = String::from_utf8(bytes)
        .map_err(|error| ApiError::bad_request(format!("response_format is not UTF-8: {error}")))?;
    parse_response_format(&value)
}

pub fn parse_response_format(value: &str) -> Result<ResponseFormat, ApiError> {
    match value.trim() {
        "" | "json" => Ok(ResponseFormat::Json),
        "text" => Ok(ResponseFormat::Text),
        other => Err(ApiError::bad_request(format!(
            "unsupported response_format {other:?}; supported values are json and text"
        ))),
    }
}

pub fn parse_openai_transcription_response(
    body: &str,
) -> codex_voice_core::TranscriptionResult<String> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|error| TranscriptionError::Request(format!("invalid JSON response: {error}")))?;
    let text = value
        .get("text")
        .and_then(|value| value.as_str())
        .ok_or_else(|| TranscriptionError::Request("response JSON did not include text".into()))?;
    Ok(text.to_string())
}

pub fn sanitize_filename(name: &str) -> String {
    let name = name.replace(['/', '\\'], "_");
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "audio.wav".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn filename_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_filename)
        .unwrap_or_else(|| "audio.wav".to_string())
}

pub fn source_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("mp3") || ext.eq_ignore_ascii_case("mpga") => {
            "audio/mpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("m4a") || ext.eq_ignore_ascii_case("mp4") => {
            "audio/mp4"
        }
        Some(ext) if ext.eq_ignore_ascii_case("webm") => "audio/webm",
        Some(ext) if ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("oga") => {
            "audio/ogg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(ext) if ext.eq_ignore_ascii_case("wav") || ext.eq_ignore_ascii_case("wave") => {
            "audio/wav"
        }
        _ => "application/octet-stream",
    }
}

pub fn join_transcripts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_transcripts_in_order() {
        let joined = join_transcripts(&[
            " first ".to_string(),
            "".to_string(),
            "second".to_string(),
            " third\n".to_string(),
        ]);
        assert_eq!(joined, "first\n\nsecond\n\nthird");
    }
}
