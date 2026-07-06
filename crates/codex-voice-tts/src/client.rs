use std::collections::HashMap;

use bytes::BytesMut;
use codex_voice_core::{
    SpeechClient, SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech,
};

use crate::config::{
    FallbackPolicy, ProviderKind, ResolvedPersona, ResolvedTtsConfig, SpeechPrepMode,
    SpeechPrepStrategy,
};
use crate::convert::{concatenate_wav_chunks, convert_speech};
use crate::elevenlabs::ElevenLabsSpeechClient;
use crate::google::GoogleSpeechClient;
use crate::speech_prep::{SpeechPrepClient, SpeechPrepContext, SpeechPrepOutput, SpeechPrepTarget};

const CHUNKED_TTS_MIN_CHARS: usize = 1_600;
const CHUNKED_TTS_MAX_CHARS: usize = 900;

/// Orchestrates TTS synthesis across configured providers with persona-aware fallback.
pub struct ConfiguredSpeechClient {
    config: ResolvedTtsConfig,
    speech_prep: Option<SpeechPrepClient>,
    google: Option<GoogleSpeechClient>,
    elevenlabs: Option<ElevenLabsSpeechClient>,
}

impl ConfiguredSpeechClient {
    pub fn try_new(config: ResolvedTtsConfig) -> Result<Self, SpeechError> {
        let google = config
            .google
            .as_ref()
            .map(|cfg| GoogleSpeechClient::new(cfg.clone()))
            .transpose()?;

        let elevenlabs = config
            .elevenlabs
            .as_ref()
            .map(|cfg| ElevenLabsSpeechClient::new(cfg.clone()))
            .transpose()?;

        let speech_prep = config
            .speech_prep
            .as_ref()
            .map(|cfg| SpeechPrepClient::new(cfg.clone()))
            .transpose()?;

        Ok(Self {
            config,
            speech_prep,
            google,
            elevenlabs,
        })
    }

    /// Resolve which persona (if any) and provider to use for a request.
    fn resolve_request<'a, 'b>(
        &'a self,
        request: &'b SpeechRequest,
    ) -> Result<(ProviderKind, Option<&'a ResolvedPersona>, Option<&'b str>), SpeechError> {
        let (persona, native_voice) = match request.voice_hint.as_deref() {
            Some(voice) => match self.config.personas.get(voice) {
                Some(persona) => (Some(persona), None),
                None => (None, Some(voice)),
            },
            None => (
                self.config
                    .default_persona
                    .as_ref()
                    .and_then(|p| self.config.personas.get(p.as_str())),
                None,
            ),
        };

        let provider = persona
            .map(|p| p.provider)
            .unwrap_or(self.config.default_provider);

        Ok((provider, persona, native_voice))
    }

    /// Determine whether an error is retryable (fallback-eligible).
    fn is_retryable(&self, error: &SpeechError) -> bool {
        match error {
            // Network, auth, and transient upstream failures are retryable.
            SpeechError::Auth(_)
            | SpeechError::RateLimited(_)
            | SpeechError::Unavailable(_)
            | SpeechError::Request(_) => true,
            // Service errors: retry only 5xx and 429, not client 4xx.
            SpeechError::Service { status, .. } => *status == 429 || *status >= 500,
            // Config and unsupported requests are terminal.
            SpeechError::Config(_) | SpeechError::Unsupported(_) | SpeechError::Message(_) => false,
        }
    }

    /// Returns true if at least one provider client was successfully created.
    pub fn has_any_provider(&self) -> bool {
        self.google.is_some() || self.elevenlabs.is_some()
    }

    /// Access the resolved TTS configuration.
    pub fn config(&self) -> &ResolvedTtsConfig {
        &self.config
    }

    fn provider_supports_inline_audio_tags(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
    ) -> bool {
        match provider {
            ProviderKind::Google => self
                .google
                .as_ref()
                .is_some_and(|client| client.supports_inline_audio_tags(request)),
            ProviderKind::ElevenLabs => self
                .elevenlabs
                .as_ref()
                .is_some_and(|client| client.supports_inline_audio_tags(request)),
        }
    }

    fn provider_model_id<'a>(
        &'a self,
        provider: ProviderKind,
        request: &'a SpeechRequest,
    ) -> String {
        match provider {
            ProviderKind::Google => self
                .google
                .as_ref()
                .map(|client| client.resolved_model_id(request).to_string())
                .unwrap_or_else(|| request.model_hint.clone()),
            ProviderKind::ElevenLabs => self
                .elevenlabs
                .as_ref()
                .and_then(|client| client.resolved_model_id(request).ok())
                .unwrap_or_else(|| request.model_hint.clone()),
        }
    }

    fn provider_speech_prep_strategy(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
    ) -> Option<SpeechPrepStrategy> {
        let prep = self.speech_prep.as_ref()?;
        let supports_inline_audio_tags =
            self.provider_supports_inline_audio_tags(provider, request);
        let model_id = self.provider_model_id(provider, request);
        let target = SpeechPrepTarget {
            provider,
            model_id: &model_id,
            supports_inline_audio_tags,
        };
        Some(prep.strategy_for_target(&target))
    }

    fn provider_max_text_length(&self, provider: ProviderKind) -> usize {
        match provider {
            ProviderKind::Google => self
                .google
                .as_ref()
                .map(|client| client.max_text_length())
                .unwrap_or(self.config.max_text_length),
            ProviderKind::ElevenLabs => self
                .elevenlabs
                .as_ref()
                .map(|client| client.max_text_length())
                .unwrap_or(self.config.max_text_length),
        }
    }

    async fn prepare_request_for_provider(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        cache: &mut HashMap<(String, String), SpeechRequest>,
    ) -> SpeechRequest {
        let Some(prep) = &self.speech_prep else {
            return request.clone();
        };

        let supports_inline_audio_tags =
            self.provider_supports_inline_audio_tags(provider, request);
        let model_id = self.provider_model_id(provider, request);
        let target = SpeechPrepTarget {
            provider,
            model_id: &model_id,
            supports_inline_audio_tags,
        };
        let context = SpeechPrepContext {
            target,
            persona,
            instructions: request.instructions.as_deref(),
        };

        let provider_limit = self.provider_max_text_length(provider);
        let fit_limit = speech_prep_fit_limit(provider_limit);
        let mut request = request.clone();
        if prep.should_shorten_to_fit(&request.input, provider_limit) {
            let cache_key = (request.input.clone(), format!("shorten-to-fit:{fit_limit}"));
            if let Some(cached) = cache.get(&cache_key) {
                request = cached.clone();
            } else {
                match prep
                    .prepare_to_fit(&request.input, context, fit_limit)
                    .await
                {
                    Ok(Some(input)) => {
                        tracing::info!(
                            original_chars = request.input.chars().count(),
                            prepared_chars = input.chars().count(),
                            provider = ?provider,
                            model = %model_id,
                            max_text_length = fit_limit,
                            "shortened TTS text to fit provider limit"
                        );
                        request = SpeechRequest {
                            input,
                            ..request.clone()
                        };
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, provider = ?provider, "speech prep failed while shortening over-limit TTS text; using extractive fallback");
                        match prep.extractive_shorten_to_fit(&request.input, fit_limit) {
                            Ok(input) => {
                                tracing::info!(
                                    original_chars = request.input.chars().count(),
                                    prepared_chars = input.chars().count(),
                                    provider = ?provider,
                                    model = %model_id,
                                    max_text_length = fit_limit,
                                    "extractively shortened TTS text after speech prep failure"
                                );
                                request = SpeechRequest {
                                    input,
                                    ..request.clone()
                                };
                            }
                            Err(fallback_error) => {
                                tracing::warn!(%fallback_error, provider = ?provider, "extractive speech prep fallback failed; using original TTS text");
                            }
                        }
                    }
                }
                cache.insert(cache_key, request.clone());
            }
        }

        if prep.mode() == SpeechPrepMode::Shorten {
            return request;
        }

        let supports_inline_audio_tags =
            self.provider_supports_inline_audio_tags(provider, &request);
        let model_id = self.provider_model_id(provider, &request);
        let target = SpeechPrepTarget {
            provider,
            model_id: &model_id,
            supports_inline_audio_tags,
        };
        let context = SpeechPrepContext {
            target,
            persona,
            instructions: request.instructions.as_deref(),
        };

        if !prep.should_prepare(&request.input, &context.target) {
            return request;
        }
        let strategy = prep.strategy_for_target(&context.target);
        let cache_key = (request.input.clone(), strategy.as_name().to_string());
        if let Some(cached) = cache.get(&cache_key) {
            return cached.clone();
        }

        let prepared_request = match prep.prepare(&request.input, context).await {
            Ok(Some(SpeechPrepOutput::Input(input))) => {
                tracing::info!(
                    original_chars = request.input.chars().count(),
                    prepared_chars = input.chars().count(),
                    provider = ?provider,
                    model = %model_id,
                    strategy = %strategy.as_name(),
                    inline_audio_tags = supports_inline_audio_tags,
                    "prepared TTS text before synthesis"
                );
                SpeechRequest {
                    input,
                    ..request.clone()
                }
            }
            Ok(Some(SpeechPrepOutput::DeliveryInstruction(instruction))) => {
                tracing::info!(
                    original_chars = request.input.chars().count(),
                    instruction_chars = instruction.chars().count(),
                    provider = ?provider,
                    model = %model_id,
                    strategy = %strategy.as_name(),
                    "prepared TTS delivery instruction before synthesis"
                );
                SpeechRequest {
                    instructions: Some(merge_instructions(
                        request.instructions.as_deref(),
                        &instruction,
                    )),
                    ..request.clone()
                }
            }
            Ok(None) => request.clone(),
            Err(error) => {
                tracing::warn!(%error, provider = ?provider, "speech prep failed; using original TTS text");
                match prep.fallback_performance_tags(&request.input, &context.target) {
                    Ok(Some(input)) => {
                        tracing::info!(
                            original_chars = request.input.chars().count(),
                            prepared_chars = input.chars().count(),
                            provider = ?provider,
                            model = %model_id,
                            strategy = %strategy.as_name(),
                            "applied local fallback performance tag after speech prep failure"
                        );
                        SpeechRequest {
                            input,
                            ..request.clone()
                        }
                    }
                    Ok(None) => request.clone(),
                    Err(fallback_error) => {
                        tracing::warn!(%fallback_error, provider = ?provider, "fallback performance tagging failed; using original TTS text");
                        request.clone()
                    }
                }
            }
        };
        cache.insert(cache_key, prepared_request.clone());
        prepared_request
    }

    /// Dispatch one synthesis request to the requested provider.
    async fn synthesize_single_with(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech> {
        match provider {
            ProviderKind::Google => {
                if let Some(client) = &self.google {
                    client.synthesize(request, persona, native_voice).await
                } else {
                    Err(SpeechError::Unavailable("Google TTS not configured".into()))
                }
            }
            ProviderKind::ElevenLabs => {
                if let Some(client) = &self.elevenlabs {
                    client.synthesize(request, persona, native_voice).await
                } else {
                    Err(SpeechError::Unavailable(
                        "ElevenLabs TTS not configured".into(),
                    ))
                }
            }
        }
    }

    /// Dispatch synthesis to the requested provider, chunking long WAV requests so providers do not
    /// have to generate several minutes of audio in a single upstream call.
    async fn synthesize_with(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech> {
        if request.format != SpeechFormat::Wav
            || request.input.chars().count() < CHUNKED_TTS_MIN_CHARS
        {
            return self
                .synthesize_single_with(provider, request, persona, native_voice)
                .await;
        }

        let chunks = split_tts_text(&request.input, CHUNKED_TTS_MAX_CHARS);
        if chunks.len() < 2 {
            return self
                .synthesize_single_with(provider, request, persona, native_voice)
                .await;
        }

        tracing::info!(
            provider = ?provider,
            chunks = chunks.len(),
            text_chars = request.input.chars().count(),
            max_chunk_chars = CHUNKED_TTS_MAX_CHARS,
            "chunking long TTS request"
        );

        let chunk_format = match provider {
            ProviderKind::ElevenLabs => SpeechFormat::Pcm,
            ProviderKind::Google => request.format,
        };
        let mut synthesized_chunks = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.into_iter().enumerate() {
            tracing::debug!(
                provider = ?provider,
                chunk_index = index,
                chunk_chars = chunk.chars().count(),
                "synthesizing TTS chunk"
            );
            let chunk_request = SpeechRequest {
                input: chunk,
                format: chunk_format,
                ..request.clone()
            };
            synthesized_chunks.push(
                self.synthesize_single_with(provider, &chunk_request, persona, native_voice)
                    .await?,
            );
        }

        match chunk_format {
            SpeechFormat::Wav => concatenate_wav_chunks(synthesized_chunks).await,
            SpeechFormat::Pcm | SpeechFormat::Mp3 => {
                let mut bytes = BytesMut::new();
                for chunk in synthesized_chunks {
                    if chunk.format != chunk_format {
                        return Err(SpeechError::Request(format!(
                            "cannot concatenate non-{} speech chunk",
                            chunk_format.to_openai()
                        )));
                    }
                    bytes.extend_from_slice(&chunk.bytes);
                }
                convert_speech(
                    SynthesizedSpeech {
                        bytes: bytes.freeze(),
                        format: chunk_format,
                        mime_type: synthesized_chunk_mime_type(chunk_format).to_string(),
                        prepared_input: None,
                    },
                    request.format,
                )
                .await
            }
            _ => unreachable!("chunked TTS only requests WAV, PCM, or MP3 chunks"),
        }
    }
}

fn synthesized_chunk_mime_type(format: SpeechFormat) -> &'static str {
    match format {
        SpeechFormat::Pcm => "audio/L16;codec=pcm;rate=24000",
        _ => format.mime_type(),
    }
}

fn speech_prep_fit_limit(provider_limit: usize) -> usize {
    provider_limit.min(4_000)
}

fn split_tts_text(input: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = input.trim();
    while remaining.chars().count() > max_chars {
        let split_at = split_index_at_or_before(remaining, max_chars);
        let (head, tail) = remaining.split_at(split_at);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_string());
        }
        remaining = tail.trim_start();
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

fn split_index_at_or_before(input: &str, max_chars: usize) -> usize {
    let hard_limit = input
        .char_indices()
        .nth(max_chars)
        .map(|(index, _)| index)
        .unwrap_or(input.len());
    let prefix = &input[..hard_limit];
    for pattern in [". ", "! ", "? ", "\n\n", "\n", "; ", ", ", " "] {
        if let Some(index) = prefix.rfind(pattern) {
            let split = index + pattern.len();
            if split > 0 {
                return split;
            }
        }
    }
    hard_limit
}

fn merge_instructions(existing: Option<&str>, generated: &str) -> String {
    match existing.map(str::trim).filter(|value| !value.is_empty()) {
        Some(existing) => format!("{existing}\n\nPer-message delivery direction: {generated}"),
        None => format!("Per-message delivery direction: {generated}"),
    }
}

#[async_trait::async_trait]
impl SpeechClient for ConfiguredSpeechClient {
    async fn synthesize(&self, request: &SpeechRequest) -> SpeechResult<SynthesizedSpeech> {
        let (primary_provider, persona, native_voice) = self.resolve_request(request)?;
        let mut prep_cache = HashMap::new();
        let primary_request = self
            .prepare_request_for_provider(primary_provider, request, persona, &mut prep_cache)
            .await;

        let primary_prepared_input =
            (primary_request.input != request.input).then(|| primary_request.input.clone());

        let primary_result = self
            .synthesize_with(primary_provider, &primary_request, persona, native_voice)
            .await;

        let primary_err = match primary_result {
            Ok(mut speech) => {
                speech.prepared_input = primary_prepared_input;
                return Ok(speech);
            }
            Err(e) if !self.is_retryable(&e) => return Err(e),
            Err(e) => e,
        };

        tracing::warn!(%primary_err, provider = ?primary_provider, "primary TTS provider failed, attempting fallback");

        // Fallback: try the other provider if persona allows it.
        if let Some(persona) = persona {
            if persona.fallback_policy == FallbackPolicy::PreservePersona {
                let fallback_provider = match primary_provider {
                    ProviderKind::Google => ProviderKind::ElevenLabs,
                    ProviderKind::ElevenLabs => ProviderKind::Google,
                };
                let fallback_request = if primary_prepared_input.is_some()
                    && self.provider_speech_prep_strategy(fallback_provider, &primary_request)
                        == Some(SpeechPrepStrategy::InlineTags)
                {
                    primary_request.clone()
                } else {
                    self.prepare_request_for_provider(
                        fallback_provider,
                        request,
                        Some(persona),
                        &mut prep_cache,
                    )
                    .await
                };
                let fallback_prepared_input = (fallback_request.input != request.input)
                    .then(|| fallback_request.input.clone());

                match self
                    .synthesize_with(fallback_provider, &fallback_request, Some(persona), None)
                    .await
                {
                    Ok(mut speech) => {
                        speech.prepared_input = fallback_prepared_input;
                        return Ok(speech);
                    }
                    Err(e) => {
                        tracing::warn!(%e, provider = ?fallback_provider, "fallback TTS provider also failed");
                    }
                }
            }
        }

        Err(SpeechError::Unavailable(format!(
            "all TTS providers failed. primary error: {}",
            primary_err
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::split_tts_text;

    #[test]
    fn split_tts_text_prefers_sentence_boundaries() {
        let chunks = split_tts_text("First sentence. Second sentence. Third sentence.", 25);

        assert_eq!(
            chunks,
            vec!["First sentence.", "Second sentence.", "Third sentence."]
        );
    }

    #[test]
    fn split_tts_text_preserves_long_words_without_empty_chunks() {
        let chunks = split_tts_text("abcdefghij klm", 5);

        assert_eq!(chunks, vec!["abcde", "fghij", "klm"]);
    }
}
