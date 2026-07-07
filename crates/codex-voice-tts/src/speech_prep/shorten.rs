use super::*;
use codex_voice_core::{SpeechError, SpeechResult};

pub(super) fn validate_shorten_output(
    input_chars: usize,
    prepared: &str,
    max_length: usize,
) -> SpeechResult<()> {
    let min_chars = shorten_min_output_chars(input_chars, max_length);
    let prepared_chars = prepared.chars().count();
    if prepared_chars < min_chars {
        return Err(SpeechError::Request(format!(
            "speech prep shortened text below minimum: {prepared_chars} below {min_chars}"
        )));
    }
    Ok(())
}

pub(super) fn shorten_or_extract(input: &str, prepared: &str, max_length: usize) -> String {
    let shortened = truncate_chars(prepared, max_length);
    if validate_shorten_output(input.chars().count(), &shortened, max_length).is_ok() {
        return shortened;
    }
    extractive_shorten_to_fit(input, max_length)
}

pub(super) fn extractive_shorten_to_fit(input: &str, max_length: usize) -> String {
    truncate_chars(input, max_length)
}
