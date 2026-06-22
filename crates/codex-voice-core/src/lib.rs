pub mod audio;
pub mod engine;
pub mod fs;
pub mod platform;
pub mod redaction;
pub mod speech;
pub mod transcription;

pub use audio::{AudioError, AudioRecorder, AudioResult, RecordedAudio};
pub use engine::{AppEvent, DictationEngine, DictationState, ErrorStage};
pub use platform::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, TextInjector,
};
pub use redaction::{redact_bearer_tokens, redact_diagnostics, redact_jwts};
pub use speech::{
    SpeechClient, SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech,
};
pub use transcription::{TranscriptionClient, TranscriptionError, TranscriptionResult};
