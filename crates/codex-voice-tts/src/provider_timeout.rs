use std::time::Duration;

const LONG_TTS_TIMEOUT_MAX: Duration = Duration::from_secs(300);

pub(crate) fn tts_timeout_for_input(base: Duration, input: &str) -> Duration {
    let chars = input.chars().count() as u64;
    if chars <= 1_200 {
        return base;
    }

    let scaled_secs = (chars / 25).clamp(90, LONG_TTS_TIMEOUT_MAX.as_secs());
    base.max(Duration::from_secs(scaled_secs))
        .min(LONG_TTS_TIMEOUT_MAX)
}

#[cfg(test)]
mod tests {
    use super::tts_timeout_for_input;
    use std::time::Duration;

    #[test]
    fn short_inputs_keep_configured_timeout() {
        let timeout = tts_timeout_for_input(Duration::from_secs(30), &"a".repeat(1_200));

        assert_eq!(timeout, Duration::from_secs(30));
    }

    #[test]
    fn long_inputs_get_scaled_timeout() {
        let timeout = tts_timeout_for_input(Duration::from_secs(30), &"a".repeat(4_000));

        assert_eq!(timeout, Duration::from_secs(160));
    }

    #[test]
    fn timeout_scaling_is_capped() {
        let timeout = tts_timeout_for_input(Duration::from_secs(30), &"a".repeat(20_000));

        assert_eq!(timeout, Duration::from_secs(300));
    }
}
