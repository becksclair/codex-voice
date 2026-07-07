use codex_voice_core::{SpeechRequest, SpeechResult, SynthesizedSpeech};

use crate::config::ResolvedPersona;

/// Uniform synthesis surface shared by the concrete TTS backends.
///
/// Collapsing the per-provider dispatch behind this trait keeps the aggregate
/// [`crate::client::ConfiguredSpeechClient`] free of paired `ProviderKind` match
/// arms, so adding a backend means implementing the trait rather than editing
/// several dispatch sites in lockstep.
#[async_trait::async_trait]
pub(crate) trait TtsProvider: Send + Sync {
    fn supports_inline_audio_tags(&self, request: &SpeechRequest) -> bool;
    fn resolved_model_id(&self, request: &SpeechRequest) -> SpeechResult<String>;
    fn max_text_length(&self) -> usize;
    async fn synthesize(
        &self,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech>;
}
