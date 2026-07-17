use codex_voice_core::SpeechError;

pub fn resolve_provider_key(
    configured_env: Option<&str>,
    primary_env: &str,
    fallback_env: &str,
) -> Result<String, SpeechError> {
    let configured_env = configured_env
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let first = configured_env.unwrap_or(primary_env);
    let value = std::env::var(first).or_else(|_| {
        if configured_env.is_some() {
            Err(std::env::VarError::NotPresent)
        } else {
            std::env::var(fallback_env)
        }
    });
    let value = value.map_err(|_| {
        let attempted = configured_env
            .map(str::to_string)
            .unwrap_or_else(|| format!("{primary_env} or {fallback_env}"));
        SpeechError::Auth(format!(
            "missing API key in environment variable {attempted}"
        ))
    })?;
    if value.trim().is_empty() {
        return Err(SpeechError::Auth(format!(
            "API key in environment variable {first} is empty"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_standard_and_custom_environment_names() {
        std::env::set_var("TEST_PROVIDER_PRIMARY", "primary");
        std::env::set_var("TEST_PROVIDER_CUSTOM", "custom");
        assert_eq!(
            resolve_provider_key(None, "TEST_PROVIDER_PRIMARY", "TEST_PROVIDER_FALLBACK").unwrap(),
            "primary"
        );
        assert_eq!(
            resolve_provider_key(
                Some("TEST_PROVIDER_CUSTOM"),
                "TEST_PROVIDER_PRIMARY",
                "TEST_PROVIDER_FALLBACK"
            )
            .unwrap(),
            "custom"
        );
    }
}
