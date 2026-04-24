use async_trait::async_trait;
use std::{path::PathBuf, time::Duration};
use thiserror::Error;

pub type AudioResult<T> = Result<T, AudioError>;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone)]
pub struct RecordedAudio {
    pub path: PathBuf,
    pub content_type: String,
    pub filename: String,
    pub duration: Duration,
}

#[async_trait]
pub trait AudioRecorder: Send + Sync {
    async fn start(&self) -> AudioResult<()>;
    async fn stop(&self) -> AudioResult<Option<RecordedAudio>>;
    async fn cancel(&self) -> AudioResult<()>;
}
