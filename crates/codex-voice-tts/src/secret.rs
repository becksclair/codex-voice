use codex_voice_core::SpeechError;

/// Resolve a secret from config or fallback environment variables.
///
/// `config_value` may be a JSON secret-ref object like
/// `{ "source": "env", "id": "ELEVENLABS_API_KEY" }` or a plain string.
/// If not present in config, falls back to `primary_env` and then `fallback_env`.
pub fn resolve_secret(
    config_value: Option<&serde_json::Value>,
    primary_env: &str,
    fallback_env: &str,
) -> Result<String, SpeechError> {
    if let Some(value) = config_value.and_then(|v| v.as_str()) {
        let value = value.trim();
        if value.is_empty() {
            return Err(SpeechError::Auth("inline API key is empty".into()));
        }
        return Ok(value.to_string());
    }

    let env_var = config_value
        .and_then(|v| {
            v.get("source")
                .and_then(|s| s.as_str())
                .filter(|s| *s == "env")
        })
        .and_then(|_| config_value.and_then(|v| v.get("id").and_then(|i| i.as_str())))
        .unwrap_or(primary_env);

    let val = std::env::var(env_var)
        .or_else(|_| std::env::var(fallback_env))
        .map_err(|_| {
            SpeechError::Auth(format!(
                "missing API key in env vars {} or {}",
                env_var, fallback_env
            ))
        })?;

    if val.trim().is_empty() {
        return Err(SpeechError::Auth(format!(
            "API key in {} or {} is empty",
            env_var, fallback_env
        )));
    }

    Ok(val)
}

/// Convenience for resolving a plain env-only key.
pub fn resolve_env_key(env_var: &str) -> Result<String, SpeechError> {
    std::env::var(env_var)
        .map_err(|_| SpeechError::Auth(format!("missing API key in env var {}", env_var)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_from_config_env_ref() {
        std::env::set_var("MY_CUSTOM_KEY", "secret123");
        let val = serde_json::json!({"source": "env", "id": "MY_CUSTOM_KEY" });
        let result = resolve_secret(Some(&val), "PRIMARY", "FALLBACK").unwrap();
        assert_eq!(result, "secret123");
    }

    #[test]
    fn resolve_from_inline_string() {
        let val = serde_json::json!("inline-secret");
        let result = resolve_secret(Some(&val), "PRIMARY", "FALLBACK").unwrap();
        assert_eq!(result, "inline-secret");
    }

    #[test]
    fn resolve_fallback_env() {
        std::env::set_var("FALLBACK_KEY", "fallback-secret");
        let result = resolve_secret(None, "MISSING_PRIMARY", "FALLBACK_KEY").unwrap();
        assert_eq!(result, "fallback-secret");
    }

    #[test]
    fn missing_env_errors() {
        std::env::remove_var("DEFINITELY_MISSING_KEY_A");
        std::env::remove_var("DEFINITELY_MISSING_KEY_B");
        let err = resolve_secret(None, "DEFINITELY_MISSING_KEY_A", "DEFINITELY_MISSING_KEY_B")
            .unwrap_err();
        assert!(err.to_string().contains("missing API key"));
    }
}
