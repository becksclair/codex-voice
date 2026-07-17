pub mod models;
pub mod provider;
pub mod serde;

use std::path::PathBuf;

use codex_voice_core::SpeechError;

pub use models::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, GooglePersonaConfig,
    GoogleRuntimeConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig, SpeechPrepConfig,
    SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies, SpeechPrepStrategy,
};

#[derive(Debug, Clone)]
pub struct VoiceConfigLoader {
    pub path: PathBuf,
}

impl VoiceConfigLoader {
    pub fn default_path() -> Result<PathBuf, SpeechError> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            SpeechError::Config("could not resolve platform config directory".into())
        })?;
        Ok(config_dir.join("codex-voice").join("config.json"))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<ResolvedTtsConfig, SpeechError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| {
            SpeechError::Config(format!("failed to read config at {:?}: {}", self.path, e))
        })?;
        let file = serde::VoiceConfigFile::parse(&raw)?;
        file.resolve()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> &'static str {
        r#"{
          "version": 1,
          "defaultVoice": "sky",
          "providers": {
            "elevenlabs": {
              "models": ["eleven_v3"],
              "textNormalization": "on",
              "streamGain": 1.7
            },
            "google": {
              "models": ["gemini-3.1-flash-tts-preview", "gemini-2.5-flash-preview-tts"]
            }
          },
          "voices": {
            "sky": {
              "label": "Sky",
              "description": "Warm test voice",
              "prompt": {
                "scene": "At home",
                "sampleContext": "Gentle and clear",
                "style": "Warm",
                "pace": "Relaxed",
                "constraints": ["Do not narrate tags."]
              },
              "backends": [
                {
                  "provider": "elevenlabs",
                  "voiceId": "test-eleven-voice",
                  "settings": {
                    "stability": 0.15,
                    "similarityBoost": 0.8,
                    "style": 0.75,
                    "speakerBoost": true,
                    "speed": 1.12
                  }
                },
                { "provider": "google", "voice": "Sulafat" }
              ]
            }
          },
          "advanced": {
            "providers": {
              "google": { "apiKeyEnv": "TEST_CONFIG_GOOGLE_KEY" },
              "elevenlabs": { "apiKeyEnv": "TEST_CONFIG_ELEVEN_KEY" }
            }
          }
        }"#
    }

    fn resolve(raw: &str) -> Result<ResolvedTtsConfig, codex_voice_core::SpeechError> {
        std::env::set_var("TEST_CONFIG_GOOGLE_KEY", "google-key");
        std::env::set_var("TEST_CONFIG_ELEVEN_KEY", "eleven-key");
        serde::VoiceConfigFile::parse(raw)?.resolve()
    }

    #[test]
    fn parses_strict_v1_config_and_preserves_effective_voice() {
        let resolved = resolve(config()).unwrap();
        assert_eq!(resolved.default_provider, ProviderKind::ElevenLabs);
        assert_eq!(resolved.default_persona.as_deref(), Some("sky"));
        assert_eq!(resolved.max_text_length, 6000);
        let google = resolved.google.unwrap();
        assert_eq!(google.models[0], "gemini-3.1-flash-tts-preview");
        assert_eq!(google.models[1], "gemini-2.5-flash-preview-tts");
        let elevenlabs = resolved.elevenlabs.unwrap();
        assert_eq!(elevenlabs.max_text_length, 5000);
        assert_eq!(elevenlabs.stream_gain, 1.7);
        let sky = resolved.personas.get("sky").unwrap();
        assert_eq!(
            sky.provider_order,
            vec![ProviderKind::ElevenLabs, ProviderKind::Google]
        );
        assert_eq!(sky.prompt_scene.as_deref(), Some("At home"));
        assert_eq!(sky.elevenlabs.as_ref().unwrap().voice_settings.speed, 1.12);
        let prep = resolved.speech_prep.unwrap();
        assert_eq!(prep.provider, SpeechPrepProviderKind::Codex);
        assert_eq!(prep.mode, SpeechPrepMode::PerformanceTags);
        assert_eq!(prep.model, "gpt-5.6-luna");
        assert_eq!(prep.threshold, 120);
        assert_eq!(prep.max_input_length, 12000);
        assert_eq!(prep.max_length, 6000);
        assert_eq!(prep.attempt_timeout, std::time::Duration::from_secs(30));
        assert_eq!(prep.timeout, std::time::Duration::from_secs(30));
    }

    #[test]
    fn rejects_legacy_and_misplaced_fields_with_json_paths() {
        let legacy = r#"{"messages":{"tts":{}}}"#;
        let error = serde::VoiceConfigFile::parse(legacy).unwrap_err();
        assert!(error.to_string().contains("$.messages"));

        let misplaced = config().replace(
            "\"streamGain\": 1.7",
            "\"streamGain\": 1.7, \"apiKey\": \"inline\"",
        );
        let error = serde::VoiceConfigFile::parse(&misplaced).unwrap_err();
        assert!(error.to_string().contains("$.providers.elevenlabs.apiKey"));
    }

    #[test]
    fn rejects_wrong_types_with_json_paths() {
        let raw = config().replace("\"models\": [\"eleven_v3\"]", "\"models\": \"eleven_v3\"");
        let error = serde::VoiceConfigFile::parse(&raw).unwrap_err();
        assert!(error.to_string().contains("$.providers.elevenlabs.models"));
    }

    #[test]
    fn rejects_duplicate_models_and_invalid_default_voice() {
        let duplicate = config().replace("[\"eleven_v3\"]", "[\"eleven_v3\", \"eleven_v3\"]");
        assert!(resolve(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("$.providers.elevenlabs.models"));

        let missing =
            config().replace("\"defaultVoice\": \"sky\"", "\"defaultVoice\": \"missing\"");
        assert!(resolve(&missing)
            .unwrap_err()
            .to_string()
            .contains("$.defaultVoice"));
    }

    #[test]
    fn rejects_empty_and_duplicate_backend_lists() {
        let empty = config().replace(
            r#"[
                {
                  "provider": "elevenlabs",
                  "voiceId": "test-eleven-voice",
                  "settings": {
                    "stability": 0.15,
                    "similarityBoost": 0.8,
                    "style": 0.75,
                    "speakerBoost": true,
                    "speed": 1.12
                  }
                },
                { "provider": "google", "voice": "Sulafat" }
              ]"#,
            "[]",
        );
        assert!(resolve(&empty)
            .unwrap_err()
            .to_string()
            .contains("$.voices.sky.backends"));

        let duplicate = config().replace(
            r#"{ "provider": "google", "voice": "Sulafat" }"#,
            r#"{ "provider": "elevenlabs", "voiceId": "other" }"#,
        );
        assert!(resolve(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate ElevenLabs backend"));
    }

    #[test]
    fn validates_advanced_timeouts_and_overrides() {
        let overridden = config().replace(
            "\"elevenlabs\": { \"apiKeyEnv\": \"TEST_CONFIG_ELEVEN_KEY\" }",
            "\"elevenlabs\": { \"apiKeyEnv\": \"TEST_CONFIG_ELEVEN_KEY\", \"maxInputChars\": 4500 }",
        );
        assert_eq!(
            resolve(&overridden)
                .unwrap()
                .elevenlabs
                .unwrap()
                .max_text_length,
            4500
        );
        let invalid = config().replace(
            "\"providers\": {\n              \"google\": { \"apiKeyEnv\": \"TEST_CONFIG_GOOGLE_KEY\" },\n              \"elevenlabs\": { \"apiKeyEnv\": \"TEST_CONFIG_ELEVEN_KEY\" }\n            }",
            "\"providers\": {\n              \"google\": { \"apiKeyEnv\": \"TEST_CONFIG_GOOGLE_KEY\" },\n              \"elevenlabs\": { \"apiKeyEnv\": \"TEST_CONFIG_ELEVEN_KEY\" }\n            },\n            \"speechPrep\": { \"attemptTimeoutMs\": 30000, \"totalTimeoutMs\": 10000 }",
        );
        assert!(resolve(&invalid)
            .unwrap_err()
            .to_string()
            .contains("$.advanced.speechPrep.attemptTimeoutMs"));
    }

    #[test]
    fn resolves_every_advanced_field_without_silent_clamping() {
        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["advanced"] = serde_json::json!({
            "providers": {
                "google": {
                    "baseUrl": "http://127.0.0.1:9001/v1beta",
                    "apiKeyEnv": "TEST_CONFIG_GOOGLE_KEY",
                    "timeoutMs": 1000,
                    "maxInputChars": 7000,
                    "inlineAudioTags": false
                },
                "elevenlabs": {
                    "baseUrl": "http://127.0.0.1:9002",
                    "apiKeyEnv": "TEST_CONFIG_ELEVEN_KEY",
                    "timeoutMs": 2000,
                    "maxInputChars": 5500,
                    "inlineAudioTags": true,
                    "outputFormat": "pcm_24000",
                    "languageCode": "en"
                }
            },
            "speechPrep": {
                "enabled": true,
                "provider": "google",
                "models": ["google/gemini-one", "google/gemini-two"],
                "mode": "shorten",
                "reasoningEffort": "low",
                "baseUrl": "http://127.0.0.1:9003/v1beta",
                "authFile": "/tmp/codex-auth.json",
                "thresholdChars": 4200,
                "maxInputChars": 12000,
                "maxOutputChars": 5000,
                "attemptTimeoutMs": 1000,
                "totalTimeoutMs": 2000,
                "strategies": {
                    "google": "style-instruction",
                    "elevenlabs": "inline-tags",
                    "default": "off"
                },
                "tagPalette": ["warmly", "softly"],
                "capPerformanceTags": true
            }
        });
        let resolved = resolve(&serde_json::to_string(&value).unwrap()).unwrap();
        let google = resolved.google.unwrap();
        assert_eq!(google.base_url, "http://127.0.0.1:9001/v1beta");
        assert_eq!(google.max_text_length, 7000);
        assert_eq!(google.timeout, std::time::Duration::from_secs(1));
        assert_eq!(google.inline_audio_tags, Some(false));
        let elevenlabs = resolved.elevenlabs.unwrap();
        assert_eq!(elevenlabs.base_url, "http://127.0.0.1:9002");
        assert_eq!(elevenlabs.max_text_length, 5500);
        assert!(elevenlabs.max_text_length_overridden);
        assert_eq!(elevenlabs.timeout, std::time::Duration::from_secs(2));
        assert_eq!(elevenlabs.output_format, "pcm_24000");
        assert_eq!(elevenlabs.language_code.as_deref(), Some("en"));
        assert_eq!(elevenlabs.inline_audio_tags, Some(true));
        let prep = resolved.speech_prep.unwrap();
        assert_eq!(prep.provider, SpeechPrepProviderKind::Google);
        assert_eq!(prep.mode, SpeechPrepMode::Shorten);
        assert_eq!(prep.model, "google/gemini-one");
        assert_eq!(prep.fallback_models, vec!["google/gemini-two"]);
        assert_eq!(prep.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(prep.base_url, "http://127.0.0.1:9003/v1beta");
        assert_eq!(prep.threshold, 4200);
        assert_eq!(prep.max_input_length, 12000);
        assert_eq!(prep.max_length, 5000);
        assert_eq!(prep.attempt_timeout, std::time::Duration::from_secs(1));
        assert_eq!(prep.timeout, std::time::Duration::from_secs(2));
        assert_eq!(prep.tag_palette, vec!["warmly", "softly"]);
        assert!(prep.cap_performance_tags);
    }

    #[test]
    fn rejects_invalid_advanced_ranges_and_relationships() {
        fn invalid_with(speech_prep: serde_json::Value) -> String {
            let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
            value["advanced"]["speechPrep"] = speech_prep;
            serde_json::to_string(&value).unwrap()
        }

        for (raw, path) in [
            (
                invalid_with(serde_json::json!({ "attemptTimeoutMs": 249 })),
                "$.advanced.speechPrep.attemptTimeoutMs",
            ),
            (
                invalid_with(serde_json::json!({
                    "attemptTimeoutMs": 2000,
                    "totalTimeoutMs": 1000
                })),
                "$.advanced.speechPrep.attemptTimeoutMs",
            ),
            (
                invalid_with(serde_json::json!({
                    "maxInputChars": 1000,
                    "maxOutputChars": 1001
                })),
                "$.advanced.speechPrep.maxOutputChars",
            ),
            (
                invalid_with(serde_json::json!({
                    "thresholdChars": 1001,
                    "maxInputChars": 1000,
                    "maxOutputChars": 1000
                })),
                "$.advanced.speechPrep.thresholdChars",
            ),
            (
                invalid_with(serde_json::json!({ "models": ["same", "same"] })),
                "$.advanced.speechPrep.models",
            ),
            (
                invalid_with(serde_json::json!({ "tagPalette": [] })),
                "$.advanced.speechPrep.tagPalette",
            ),
        ] {
            let error = resolve(&raw).unwrap_err().to_string();
            assert!(error.contains(path), "expected {path} in {error}");
        }

        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["advanced"]["providers"]["google"]["baseUrl"] = serde_json::json!("ftp://bad");
        assert!(resolve(&serde_json::to_string(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("$.advanced.providers.google.baseUrl"));

        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["advanced"]["providers"]["google"]["apiKeyEnv"] = serde_json::json!("1BAD");
        assert!(resolve(&serde_json::to_string(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("$.advanced.providers.google.apiKeyEnv"));

        for (speech_prep, path) in [
            (
                serde_json::json!({ "enabled": false, "baseUrl": "ftp://invalid" }),
                "$.advanced.speechPrep.baseUrl",
            ),
            (
                serde_json::json!({ "enabled": false, "attemptTimeoutMs": 1 }),
                "$.advanced.speechPrep.attemptTimeoutMs",
            ),
            (
                serde_json::json!({ "enabled": false, "reasoningEffort": "" }),
                "$.advanced.speechPrep.reasoningEffort",
            ),
        ] {
            let error = resolve(&invalid_with(speech_prep)).unwrap_err().to_string();
            assert!(error.contains(path), "expected {path} in {error}");
        }
    }

    #[test]
    fn rejects_orphaned_advanced_provider_overrides() {
        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["providers"]
            .as_object_mut()
            .unwrap()
            .remove("elevenlabs");
        let error = resolve(&serde_json::to_string(&value).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("$.advanced.providers.elevenlabs"));
    }

    #[test]
    fn rejects_missing_fields_openclaw_secrets_and_misplaced_stream_gain() {
        for (raw, path) in [
            (
                r#"{"defaultVoice":"sky","providers":{},"voices":{}}"#,
                "$.version",
            ),
            (
                r#"{"version":1,"providers":{},"voices":{}}"#,
                "$.defaultVoice",
            ),
            (
                r#"{"version":1,"defaultVoice":"sky","providers":{"google":{}},"voices":{}}"#,
                "$.providers.google.models",
            ),
        ] {
            assert!(serde::VoiceConfigFile::parse(raw)
                .unwrap_err()
                .to_string()
                .contains(path));
        }

        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["providers"]["google"]["apiKey"] =
            serde_json::json!({ "source": "env", "id": "GOOGLE_API_KEY" });
        assert!(
            serde::VoiceConfigFile::parse(&serde_json::to_string(&value).unwrap())
                .unwrap_err()
                .to_string()
                .contains("$.providers.google.apiKey")
        );

        let mut value: serde_json::Value = serde_json::from_str(config()).unwrap();
        value["providers"]["google"]["streamGain"] = serde_json::json!(1.7);
        assert!(
            serde::VoiceConfigFile::parse(&serde_json::to_string(&value).unwrap())
                .unwrap_err()
                .to_string()
                .contains("$.providers.google.streamGain")
        );
    }

    #[test]
    fn checked_in_example_resolves() {
        std::env::set_var("GEMINI_API_KEY", "google-key");
        std::env::set_var("ELEVENLABS_API_KEY", "eleven-key");
        let text = include_str!("../../../../docs/codex-voice-config.example.json");
        serde::VoiceConfigFile::parse(text)
            .unwrap()
            .resolve()
            .unwrap();
    }
}
