pub mod models;
pub mod provider;
pub mod serde;

use std::path::PathBuf;

use codex_voice_core::SpeechError;

pub use models::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, FallbackPolicy,
    GooglePersonaConfig, GoogleRuntimeConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig,
    SpeechPrepConfig,
};

#[derive(Debug, Clone)]
pub struct ReadAloudConfigLoader {
    pub path: PathBuf,
}

impl ReadAloudConfigLoader {
    pub fn default_path() -> Result<PathBuf, SpeechError> {
        let home = dirs::home_dir()
            .ok_or_else(|| SpeechError::Config("could not resolve home directory".into()))?;
        Ok(home.join(".codex").join("read-aloud-defaults.json"))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<ResolvedTtsConfig, SpeechError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| {
            SpeechError::Config(format!("failed to read config at {:?}: {}", self.path, e))
        })?;
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(&raw)
            .map_err(|e| SpeechError::Config(format!("failed to parse config: {}", e)))?;
        file.resolve()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config() -> &'static str {
        r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_MINIMAL" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#
    }

    #[test]
    fn parse_minimal_google_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, minimal_config()).unwrap();

        std::env::set_var("TEST_GOOGLE_KEY_MINIMAL", "test-google-key-value");
        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();

        assert_eq!(resolved.default_provider, ProviderKind::Google);
        assert_eq!(resolved.default_persona.as_deref(), None);
        assert_eq!(resolved.max_text_length, 1000);
        assert!(resolved.google.is_some());
        assert!(resolved.elevenlabs.is_none());

        let google = resolved.google.unwrap();
        assert_eq!(google.api_key, "test-google-key-value");
        assert_eq!(google.voice, "Sulafat");
        assert_eq!(google.model, "gemini-2.5-flash-preview-tts");
    }

    #[test]
    fn resolve_voice_sky_to_persona() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");

        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "elevenlabs",
                    "persona": "sky",
                    "providers": {
                        "elevenlabs": {
                            "apiKey": { "source": "env", "id": "TEST_ELEVEN_KEY" },
                            "baseUrl": "https://api.elevenlabs.io",
                            "modelId": "eleven_v3"
                        },
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    },
                    "personas": {
                        "sky": {
                            "label": "Sky",
                            "description": "Warm and playful",
                            "provider": "elevenlabs",
                            "fallbackPolicy": "preserve-persona",
                            "providers": {
                                "elevenlabs": {
                                    "voiceId": "2tM0Teq5Piex0mNtlZnm",
                                    "voiceSettings": { "stability": 0.5, "similarityBoost": 0.75, "style": 0.3, "useSpeakerBoost": true, "speed": 1.2 }
                                },
                                "google": {
                                    "voiceName": "Sulafat",
                                    "promptTemplate": "audio-profile-v1",
                                    "personaPrompt": "Build Sky's spoken delivery..."
                                }
                            }
                        }
                    }
                }
            }
        }
        "#;
        std::fs::write(&path, config).unwrap();
        std::env::set_var("TEST_ELEVEN_KEY", "test-eleven-key");
        std::env::set_var("TEST_GOOGLE_KEY", "test-google-key");

        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();

        assert_eq!(resolved.default_provider, ProviderKind::ElevenLabs);
        let sky = resolved.personas.get("sky").expect("sky persona missing");
        assert_eq!(sky.provider, ProviderKind::ElevenLabs);
        assert_eq!(sky.fallback_policy, FallbackPolicy::PreservePersona);
        assert!(sky.elevenlabs.is_some());
        assert!(sky.google.is_some());

        let eleven = sky.elevenlabs.as_ref().unwrap();
        assert_eq!(eleven.voice_id, "2tM0Teq5Piex0mNtlZnm");
        assert_eq!(eleven.voice_settings.speed, 1.2);
    }

    #[test]
    fn missing_tts_block_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, r#"{"messages": {}}"#).unwrap();
        let loader = ReadAloudConfigLoader::new(path);
        let err = loader.load().expect_err("should fail");
        assert!(err.to_string().contains("missing messages.tts block"));
    }

    #[test]
    fn rejects_unusable_default_provider() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "elevenlabs",
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_UNUSABLE_DEFAULT" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var("TEST_GOOGLE_KEY_UNUSABLE_DEFAULT", "key");
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();
        let err = file
            .resolve()
            .expect_err("default provider should be unusable");
        assert!(err
            .to_string()
            .contains("ElevenLabs is selected but not configured"));
    }

    #[test]
    fn max_text_length_enforced_at_parse_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, minimal_config()).unwrap();
        std::env::set_var("TEST_GOOGLE_KEY_MINIMAL", "test-google-key-value");

        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();
        assert_eq!(resolved.max_text_length, 1000);
    }

    #[test]
    fn provider_timeout_is_capped_to_thirty_seconds() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "timeoutMs": 120000,
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_TIMEOUT_CAP" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var("TEST_GOOGLE_KEY_TIMEOUT_CAP", "test-google-key-value");
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();

        let resolved = file.resolve().unwrap();

        assert_eq!(resolved.timeout, std::time::Duration::from_secs(30));
        assert_eq!(
            resolved.google.unwrap().timeout,
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn parses_google_speech_prep_config() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "speechPrep": {
                        "enabled": true,
                        "provider": "google",
                        "model": "google/gemini-3-flash-preview",
                        "timeoutMs": 20000,
                        "threshold": 700,
                        "maxInputLength": 9000,
                        "maxLength": 420
                    },
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_SPEECH_PREP" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var("TEST_GOOGLE_KEY_SPEECH_PREP", "test-google-key-value");
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();

        let resolved = file.resolve().unwrap();
        let speech_prep = resolved.speech_prep.expect("speech prep missing");

        assert_eq!(speech_prep.provider, ProviderKind::Google);
        assert_eq!(speech_prep.model, "google/gemini-3-flash-preview");
        assert_eq!(speech_prep.threshold, 700);
        assert_eq!(speech_prep.max_input_length, 9000);
        assert_eq!(speech_prep.max_length, 420);
        assert_eq!(speech_prep.timeout, std::time::Duration::from_secs(20));
    }

    #[test]
    fn speech_prep_inherits_google_provider_credentials_and_base_url() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "speechPrep": {
                        "enabled": true,
                        "provider": "google"
                    },
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_SPEECH_PREP_INHERIT" },
                            "baseUrl": "https://google.example.test/v1beta",
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var(
            "TEST_GOOGLE_KEY_SPEECH_PREP_INHERIT",
            "test-google-key-value",
        );
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();

        let resolved = file.resolve().unwrap();
        let speech_prep = resolved.speech_prep.expect("speech prep missing");

        assert_eq!(speech_prep.api_key, "test-google-key-value");
        assert_eq!(speech_prep.base_url, "https://google.example.test/v1beta");
    }

    #[test]
    fn speech_prep_output_length_is_capped_to_tts_max_text_length() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "maxTextLength": 300,
                    "speechPrep": {
                        "enabled": true,
                        "provider": "google",
                        "maxLength": 800
                    },
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_SPEECH_PREP_OUTPUT_CAP" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var(
            "TEST_GOOGLE_KEY_SPEECH_PREP_OUTPUT_CAP",
            "test-google-key-value",
        );
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();

        let resolved = file.resolve().unwrap();
        let speech_prep = resolved.speech_prep.expect("speech prep missing");

        assert_eq!(resolved.max_text_length, 300);
        assert_eq!(speech_prep.max_length, 300);
    }

    #[test]
    fn speech_prep_threshold_is_capped_to_tts_max_text_length() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "maxTextLength": 300,
                    "speechPrep": {
                        "enabled": true,
                        "provider": "google"
                    },
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_SPEECH_PREP_THRESHOLD_CAP" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var(
            "TEST_GOOGLE_KEY_SPEECH_PREP_THRESHOLD_CAP",
            "test-google-key-value",
        );
        let file: serde::ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();

        let resolved = file.resolve().unwrap();
        let speech_prep = resolved.speech_prep.expect("speech prep missing");

        assert_eq!(resolved.max_text_length, 300);
        assert_eq!(speech_prep.threshold, 300);
    }
}
