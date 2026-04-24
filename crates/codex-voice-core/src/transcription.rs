use crate::RecordedAudio;
use async_trait::async_trait;
use thiserror::Error;

pub type TranscriptionResult<T> = Result<T, TranscriptionError>;

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error("{0}")]
    Message(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("request failed: {0}")]
    Request(String),
}

#[async_trait]
pub trait TranscriptionClient: Send + Sync {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String>;
}
