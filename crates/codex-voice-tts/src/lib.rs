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
pub use config::{ProviderKind, ReadAloudConfigLoader, ResolvedPersona, ResolvedTtsConfig};
pub use sanitize::sanitize_for_tts;
pub use secret::resolve_secret;
pub use speech_prep::{collect_bracket_tags, SpeechPrepClient};
