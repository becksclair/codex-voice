pub type SpeechResult<T> = Result<T, SpeechError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechFormat {
    Mp3,
    Opus,
    Aac,
    Flac,
    Wav,
    Pcm,
}

impl SpeechFormat {
    pub fn from_openai(s: &str) -> Option<Self> {
        match s {
            "mp3" => Some(Self::Mp3),
            "opus" => Some(Self::Opus),
            "aac" => Some(Self::Aac),
            "flac" => Some(Self::Flac),
            "wav" => Some(Self::Wav),
            "pcm" => Some(Self::Pcm),
            _ => None,
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/opus",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/L16",
        }
    }

    /// Returns the lowercase OpenAI-style format string.
    pub fn to_openai(&self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm => "pcm",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeechRequest {
    pub input: String,
    pub provider_hint: Option<String>,
    pub model_hint: String,
    pub voice_hint: Option<String>,
    pub speech_prep_enabled: Option<bool>,
    pub speech_prep_model_hint: Option<String>,
    pub speech_prep_reasoning_effort: Option<String>,
    pub speech_prep_timeout_ms: Option<u64>,
    pub instructions: Option<String>,
    pub format: SpeechFormat,
    pub speed: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct SynthesizedSpeech {
    pub bytes: bytes::Bytes,
    pub format: SpeechFormat,
    pub mime_type: String,
    pub prepared_input: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("{0}")]
    Message(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("unsupported request: {0}")]
    Unsupported(String),
    #[error("request failed: {0}")]
    Request(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("service unavailable: {0}")]
    Unavailable(String),
    #[error("service responded with HTTP {status}: {message}")]
    Service { status: u16, message: String },
}

#[async_trait::async_trait]
pub trait SpeechClient: Send + Sync {
    async fn prepare(&self, request: &SpeechRequest) -> SpeechResult<String> {
        Ok(request.input.clone())
    }

    async fn synthesize(&self, request: &SpeechRequest) -> SpeechResult<SynthesizedSpeech>;
}
