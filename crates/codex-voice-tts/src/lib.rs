pub mod client;
pub mod config;
pub mod convert;
pub mod elevenlabs;
pub mod google;
pub mod sanitize;
pub mod secret;

pub use client::ConfiguredSpeechClient;
pub use config::{ProviderKind, ReadAloudConfigLoader, ResolvedPersona, ResolvedTtsConfig};
pub use sanitize::sanitize_for_tts;
pub use secret::resolve_secret;
