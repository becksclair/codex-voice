use super::*;
use codex_voice_core::{SpeechError, SpeechResult};

pub(super) fn validate_performance_tags_output(original: &str, prepared: &str) -> SpeechResult<()> {
    let tags = collect_bracket_tags(prepared);
    if tags.is_empty() && original.trim() != prepared.trim() {
        return Err(SpeechError::Request(
            "speech prep added performance direction without square-bracket tags".into(),
        ));
    }
    let word_count = words_without_tags(original).len().max(1);
    let max_tags = word_count.div_ceil(40).clamp(2, 16);
    if tags.len() > max_tags {
        return Err(SpeechError::Request(format!(
            "speech prep returned too many performance tags: {} above max {}",
            tags.len(),
            max_tags
        )));
    }

    let preserve = preservation_ratio(original, prepared);
    let original_words = words_without_tags(original);
    let prepared_words = words_without_tags(prepared);
    let tail_preserved = original_words
        .last()
        .is_none_or(|tail| prepared_words.iter().any(|word| word == tail));
    if preserve < 0.97 || !tail_preserved {
        return Err(SpeechError::Request(format!(
            "speech prep changed text too much: preservation ratio {:.3}",
            preserve
        )));
    }
    Ok(())
}

pub(super) fn repair_bare_leading_performance_cue(
    original: &str,
    prepared: &str,
    tag_palette: &[String],
) -> String {
    if original.trim() == prepared.trim() {
        return prepared.to_string();
    }

    let prepared = repair_leading_bare_cue(original, prepared, tag_palette);
    repair_sentence_boundary_bare_cues(original, &prepared, tag_palette)
}

pub(super) fn repair_sentence_boundary_bare_cues(
    original: &str,
    prepared: &str,
    tag_palette: &[String],
) -> String {
    let phrases = bare_performance_cue_phrases(tag_palette);
    if phrases.is_empty() {
        return prepared.to_string();
    }

    // `phrases` can include user palette phrases with non-ASCII letters, so
    // fold with full Unicode case rules to match `bare_performance_cue_phrases`.
    let original_lower = original.to_lowercase();
    let mut repaired = prepared.to_string();
    for _ in 0..8 {
        let Some((start, phrase_len, after_len, phrase)) =
            find_sentence_boundary_bare_cue(&repaired, &phrases, &original_lower)
        else {
            break;
        };
        let candidate = format!(
            "{}[{}] {}",
            &repaired[..start],
            phrase,
            repaired[start + phrase_len + after_len..].trim_start()
        );
        if preservation_ratio(original, &candidate) >= 0.97 {
            repaired = candidate;
        } else {
            break;
        }
    }

    repaired
}

pub(super) fn find_sentence_boundary_bare_cue(
    text: &str,
    phrases: &[String],
    original_lower: &str,
) -> Option<(usize, usize, usize, String)> {
    for (start, _) in text.char_indices() {
        if !is_sentence_boundary(text, start) || is_inside_bracket_tag(text, start) {
            continue;
        }
        let rest = &text[start..];
        for phrase in phrases {
            if original_lower.contains(phrase) {
                continue;
            }
            let Some(after) = strip_prefix_ignore_case(rest, phrase) else {
                continue;
            };
            // The matched prefix length in `rest` can differ from
            // `phrase.len()` under Unicode case folding (e.g. some
            // characters expand when lowercased), so derive it from the
            // actual match rather than assuming byte-length parity.
            let matched_len = rest.len() - after.len();
            let after_len = cue_trailing_delimiter_len(after)?;
            return Some((start, matched_len, after_len, phrase.clone()));
        }
    }
    None
}

pub(super) fn is_sentence_boundary(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let prefix = &text[..index];
    let mut chars = prefix.chars().rev();
    let mut skipped_newline = false;
    while matches!(chars.clone().next(), Some(ch) if ch.is_whitespace()) {
        skipped_newline |= chars.next() == Some('\n');
    }
    if skipped_newline {
        return true;
    }
    matches!(chars.next(), Some('.') | Some('!') | Some('?') | Some('\n'))
}
