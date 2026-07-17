pub mod client;
mod codex_llm;
pub mod config;
pub mod convert;
pub mod elevenlabs;
pub mod google;
mod provider;
mod provider_timeout;
pub mod sanitize;
pub mod secret;
mod speech_prep;

pub use client::ConfiguredSpeechClient;
pub use codex_llm::{
    read_codex_auth_snapshot, sync_codex_auth_snapshot, CodexAuthSnapshot, CodexAuthSyncResult,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL,
};
pub use config::{ProviderKind, ResolvedPersona, ResolvedTtsConfig, VoiceConfigLoader};
pub use sanitize::sanitize_for_tts;
pub use speech_prep::{collect_bracket_tags, SpeechPrepClient};
