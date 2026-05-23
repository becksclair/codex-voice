use codex_voice_core::{SpeechClient, SpeechError, SpeechRequest, SpeechResult, SynthesizedSpeech};

use crate::config::{FallbackPolicy, ProviderKind, ResolvedPersona, ResolvedTtsConfig};
use crate::elevenlabs::ElevenLabsSpeechClient;
use crate::google::GoogleSpeechClient;

/// Orchestrates TTS synthesis across configured providers with persona-aware fallback.
pub struct ConfiguredSpeechClient {
    config: ResolvedTtsConfig,
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

        Ok(Self {
            config,
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

    /// Dispatch synthesis to the requested provider.
    async fn synthesize_with(
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
}

#[async_trait::async_trait]
impl SpeechClient for ConfiguredSpeechClient {
    async fn synthesize(&self, request: &SpeechRequest) -> SpeechResult<SynthesizedSpeech> {
        let (primary_provider, persona, native_voice) = self.resolve_request(request)?;

        let primary_result = self
            .synthesize_with(primary_provider, request, persona, native_voice)
            .await;

        let primary_err = match primary_result {
            Ok(speech) => return Ok(speech),
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

                match self
                    .synthesize_with(fallback_provider, request, Some(persona), None)
                    .await
                {
                    Ok(speech) => return Ok(speech),
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
