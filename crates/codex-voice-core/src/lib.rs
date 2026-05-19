pub mod audio;
pub mod engine;
pub mod platform;
pub mod redaction;
pub mod transcription;

pub use audio::{AudioError, AudioRecorder, AudioResult, RecordedAudio};
pub use engine::{AppEvent, DictationEngine, DictationState};
pub use platform::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, TextInjector,
};
pub use redaction::{redact_bearer_tokens, redact_jwts};
pub use transcription::{TranscriptionClient, TranscriptionError, TranscriptionResult};
