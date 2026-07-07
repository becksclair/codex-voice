use std::collections::HashMap;

use codex_voice_core::{
    SpeechClient, SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech,
};

use crate::config::{
    FallbackPolicy, ProviderKind, ResolvedPersona, ResolvedTtsConfig, SpeechPrepMode,
    SpeechPrepStrategy,
};
use crate::convert::{concatenate_pcm_chunks, concatenate_wav_chunks, convert_speech};
use crate::elevenlabs::ElevenLabsSpeechClient;
use crate::google::GoogleSpeechClient;
use crate::provider::TtsProvider;
use crate::speech_prep::{SpeechPrepClient, SpeechPrepContext, SpeechPrepOutput, SpeechPrepTarget};

const CHUNKED_TTS_MIN_CHARS: usize = 1_600;
const CHUNKED_TTS_MAX_CHARS: usize = 900;

/// Provider synthesis requests in flight per chunked TTS request.
const CHUNKED_TTS_CONCURRENCY: usize = 3;

/// Orchestrates TTS synthesis across configured providers with persona-aware fallback.
pub struct ConfiguredSpeechClient {
    config: ResolvedTtsConfig,
    speech_prep: Option<SpeechPrepClient>,
    google: Option<Box<dyn TtsProvider>>,
    elevenlabs: Option<Box<dyn TtsProvider>>,
}

impl ConfiguredSpeechClient {
    pub fn try_new(config: ResolvedTtsConfig) -> Result<Self, SpeechError> {
        let google = config
            .google
            .as_ref()
            .map(|cfg| GoogleSpeechClient::new(cfg.clone()))
            .transpose()?
            .map(|client| Box::new(client) as Box<dyn TtsProvider>);

        let elevenlabs = config
            .elevenlabs
            .as_ref()
            .map(|cfg| ElevenLabsSpeechClient::new(cfg.clone()))
            .transpose()?
            .map(|client| Box::new(client) as Box<dyn TtsProvider>);

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

    /// Look up the configured provider client for `kind`, if any.
    fn provider_opt(&self, kind: ProviderKind) -> Option<&dyn TtsProvider> {
        match kind {
            ProviderKind::Google => self.google.as_deref(),
            ProviderKind::ElevenLabs => self.elevenlabs.as_deref(),
        }
    }

    /// Look up the configured provider client for `kind`, erroring with the
    /// backend-specific "not configured" message when it is absent.
    fn provider(&self, kind: ProviderKind) -> SpeechResult<&dyn TtsProvider> {
        self.provider_opt(kind).ok_or_else(|| {
            // Debug renders the variant name ("Google"/"ElevenLabs"), preserving
            // the exact per-provider "not configured" messages verbatim.
            SpeechError::Unavailable(format!("{kind:?} TTS not configured"))
        })
    }

    fn provider_supports_inline_audio_tags(
        &self,
        provider: ProviderKind,
        request: &SpeechRequest,
    ) -> bool {
        self.provider_opt(provider)
            .is_some_and(|client| client.supports_inline_audio_tags(request))
    }

    fn provider_model_id(&self, provider: ProviderKind, request: &SpeechRequest) -> String {
        self.provider_opt(provider)
            .and_then(|client| client.resolved_model_id(request).ok())
            .unwrap_or_else(|| request.model_hint.clone())
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
        self.provider_opt(provider)
            .map(|client| client.max_text_length())
            .unwrap_or(self.config.max_text_length)
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
        self.provider(provider)?
            .synthesize(request, persona, native_voice)
            .await
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
        let synthesized_chunks = synthesize_ordered(
            chunks.into_iter().enumerate(),
            CHUNKED_TTS_CONCURRENCY,
            |(index, chunk)| {
                let chunk_request = SpeechRequest {
                    input: chunk,
                    format: chunk_format,
                    ..request.clone()
                };
                async move {
                    tracing::debug!(
                        provider = ?provider,
                        chunk_index = index,
                        chunk_chars = chunk_request.input.chars().count(),
                        "synthesizing TTS chunk"
                    );
                    self.synthesize_single_with(provider, &chunk_request, persona, native_voice)
                        .await
                }
            },
        )
        .await?;

        match chunk_format {
            SpeechFormat::Wav => concatenate_wav_chunks(synthesized_chunks).await,
            SpeechFormat::Pcm => concatenate_pcm_chunks(synthesized_chunks).await,
            SpeechFormat::Mp3 => {
                convert_speech(
                    concatenate_encoded_chunks(synthesized_chunks, chunk_format)?,
                    request.format,
                )
                .await
            }
            _ => unreachable!("chunked TTS only requests WAV, PCM, or MP3 chunks"),
        }
    }
}

/// Runs `f` over `items` with at most `concurrency` invocations in flight at once,
/// preserving the input order in the returned `Vec`. Fails fast on the first error,
/// mirroring the behavior of a serial `for` loop that awaits each item in turn.
async fn synthesize_ordered<I, F, Fut, T, E>(
    items: I,
    concurrency: usize,
    f: F,
) -> Result<Vec<T>, E>
where
    I: IntoIterator,
    F: FnMut(I::Item) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    use futures_util::stream::{self, StreamExt, TryStreamExt};

    stream::iter(items)
        .map(f)
        .buffered(concurrency)
        .try_collect()
        .await
}

fn synthesized_chunk_mime_type(format: SpeechFormat) -> &'static str {
    match format {
        SpeechFormat::Pcm => "audio/L16;codec=pcm;rate=24000",
        _ => format.mime_type(),
    }
}

fn concatenate_encoded_chunks(
    chunks: Vec<SynthesizedSpeech>,
    format: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    let mut bytes = bytes::BytesMut::new();
    for chunk in chunks {
        if chunk.format != format {
            return Err(SpeechError::Request(format!(
                "cannot concatenate non-{} speech chunk",
                format.to_openai()
            )));
        }
        bytes.extend_from_slice(&chunk.bytes);
    }
    Ok(SynthesizedSpeech {
        bytes: bytes.freeze(),
        format,
        mime_type: synthesized_chunk_mime_type(format).to_string(),
        prepared_input: None,
    })
}

fn speech_prep_fit_limit(provider_limit: usize) -> usize {
    provider_limit.min(4_000)
}

fn split_tts_text(input: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = input.trim();
    while remaining.chars().nth(max_chars).is_some() {
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
    use super::{
        split_index_at_or_before, split_tts_text, synthesize_ordered, ConfiguredSpeechClient,
    };
    use crate::config::{GoogleRuntimeConfig, ProviderKind, ResolvedTtsConfig};
    use codex_voice_core::SpeechError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn google_only_config() -> ResolvedTtsConfig {
        ResolvedTtsConfig {
            default_provider: ProviderKind::Google,
            default_persona: None,
            max_text_length: 1_000,
            timeout: Duration::from_secs(120),
            speech_prep: None,
            google: Some(GoogleRuntimeConfig {
                api_key: "test-key".to_string(),
                base_url: "https://example.invalid".to_string(),
                voice: "Sulafat".to_string(),
                model: "gemini-2.5-flash-preview-tts".to_string(),
                fallback_models: vec![],
                inline_audio_tags: None,
                max_text_length: 1_000,
                timeout: Duration::from_secs(120),
                scene: None,
                sample_context: None,
                style: None,
                pace: None,
                constraints: vec![],
            }),
            elevenlabs: None,
            personas: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn provider_lookup_returns_not_configured_error_for_missing_provider() {
        let client = ConfiguredSpeechClient::try_new(google_only_config())
            .expect("configured client should build with only Google present");

        // The configured provider resolves; the absent one yields the exact
        // pre-refactor "not configured" message.
        assert!(client.provider(ProviderKind::Google).is_ok());

        match client.provider(ProviderKind::ElevenLabs) {
            Err(SpeechError::Unavailable(message)) => {
                assert_eq!(message, "ElevenLabs TTS not configured");
            }
            Err(other) => panic!("expected Unavailable not-configured error, got {other:?}"),
            Ok(_) => panic!("expected an error for the unconfigured provider"),
        }
    }

    #[tokio::test]
    async fn synthesize_ordered_preserves_order_with_reversed_latencies() {
        // The first item is the slowest, so a naive unordered-completion collector
        // would return results out of order; `synthesize_ordered` must not.
        let items = vec![(0usize, 30u64), (1, 20), (2, 10), (3, 0)];

        let result: Result<Vec<usize>, ()> =
            synthesize_ordered(items, 4, |(index, delay_ms)| async move {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                Ok(index)
            })
            .await;

        assert_eq!(result.unwrap(), vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn synthesize_ordered_runs_with_bounded_concurrency() {
        let active = Arc::new(AtomicUsize::new(0));
        let high_water = Arc::new(AtomicUsize::new(0));
        let items: Vec<usize> = (0..8).collect();

        let result: Result<Vec<usize>, ()> = synthesize_ordered(items, 3, |index| {
            let active = Arc::clone(&active);
            let high_water = Arc::clone(&high_water);
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                high_water.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(index)
            }
        })
        .await;

        assert_eq!(result.unwrap(), (0..8).collect::<Vec<_>>());
        assert!(
            high_water.load(Ordering::SeqCst) >= 2,
            "expected concurrent execution, high water mark was {}",
            high_water.load(Ordering::SeqCst)
        );
        assert!(
            high_water.load(Ordering::SeqCst) <= 3,
            "concurrency exceeded configured bound, high water mark was {}",
            high_water.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn synthesize_ordered_fails_fast_on_first_error() {
        let items: Vec<usize> = (0..4).collect();

        let result: Result<Vec<usize>, &'static str> =
            synthesize_ordered(items, 2, |index| async move {
                if index == 1 {
                    Err("boom")
                } else {
                    Ok(index)
                }
            })
            .await;

        assert_eq!(result, Err("boom"));
    }

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

    // Naive reference implementation using the original loop condition
    // (`chars().count() > max`). Used to pin that the bounded condition
    // produces identical chunk vectors.
    fn split_tts_text_naive(input: &str, max_chars: usize) -> Vec<String> {
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

    #[test]
    fn split_matches_naive_count_semantics() {
        let inputs = [
            "",
            "          ",
            "exactly=10",  // exactly max (10 chars)
            "exactly+11c", // max + 1 (11 chars)
            "one two three four five six seven eight",
            "emoji 😀😀😀 mixed 🎉 text with multibyte αβγδ chars",
            "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀",
            "word   \t\n   with     long    whitespace     runs    here",
            "αβγδεζηθικλμνξοπρστυφχψω",
            "a b c d e f g h i j k l m n o p q r s t u v w x y z",
        ];
        for input in inputs {
            assert_eq!(
                split_tts_text(input, 10),
                split_tts_text_naive(input, 10),
                "mismatch for input {input:?}"
            );
        }
    }

    #[test]
    fn split_handles_large_input_quickly() {
        // 500_000-char ASCII with spaces every 10 chars.
        let mut input = String::with_capacity(500_000);
        while input.len() < 500_000 {
            input.push_str("abcdefghi ");
        }
        input.truncate(500_000);

        let chunks = split_tts_text(&input, 900);
        assert_eq!(chunks, split_tts_text_naive(&input, 900));
    }
}
