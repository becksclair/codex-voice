use codex_voice_core::SpeechError;

/// Deterministic text cleanup for TTS input.
///
/// - Trims leading/trailing whitespace
/// - Normalizes CRLF to LF
/// - Removes NUL and illegal control characters
/// - Rejects empty input after trim
/// - Enforces max text length
pub fn sanitize_for_tts(input: &str, max_length: usize) -> Result<String, SpeechError> {
    let trimmed = input.trim();
    let mut cleaned = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            // \r\n → single \n; lone \r → \n
            cleaned.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else if !c.is_control() || c == '\n' || c == '\t' {
            cleaned.push(c);
        }
    }

    cleaned.truncate(cleaned.trim_end().len());
    if cleaned.is_empty() {
        return Err(SpeechError::Unsupported(
            "input text is empty after sanitization".into(),
        ));
    }

    let char_count = cleaned.chars().count();
    if char_count > max_length {
        return Err(SpeechError::Unsupported(format!(
            "input text is {} characters, above max {}",
            char_count, max_length
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
