use codex_voice_core::SpeechError;

/// Deterministic text cleanup for TTS input.
///
/// - Trims leading/trailing whitespace
/// - Normalizes CRLF to LF
/// - Removes NUL and illegal control characters
/// - Rejects empty input after trim
/// - Enforces max text length
pub fn sanitize_for_tts(input: &str, max_length: usize) -> Result<String, SpeechError> {
    let mut cleaned = input.trim().replace("\r\n", "\n").replace('\r', "\n");

    cleaned.retain(|c| !c.is_control() || c == '\n' || c == '\t');
    cleaned = cleaned.trim().to_string();

    if cleaned.is_empty() {
        return Err(SpeechError::Unsupported(
            "input text is empty after sanitization".into(),
        ));
    }

    if cleaned.chars().count() > max_length {
        return Err(SpeechError::Unsupported(format!(
            "input text is {} characters, above max {}",
            cleaned.chars().count(),
            max_length
        )));
    }

    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_sanitize() {
        let result = sanitize_for_tts("  hello world  ", 100).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn crlf_normalized() {
        let result = sanitize_for_tts("line1\r\nline2\rline3", 100).unwrap();
        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn control_chars_removed() {
        let result = sanitize_for_tts("hello\0world\x07", 100).unwrap();
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn only_tab_and_newline_controls_are_preserved() {
        let result = sanitize_for_tts("a\tb\nc\x0bd", 100).unwrap();
        assert_eq!(result, "a\tb\ncd");
    }

    #[test]
    fn empty_rejected() {
        let err = sanitize_for_tts("   ", 100).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn max_length_enforced() {
        let text = "a".repeat(101);
        let err = sanitize_for_tts(&text, 100).unwrap_err();
        assert!(err.to_string().contains("above max"));
    }
}
