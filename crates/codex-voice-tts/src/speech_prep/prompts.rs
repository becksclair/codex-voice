use super::*;
use crate::config::{SpeechPrepMode, SpeechPrepStrategy};

pub(super) fn build_prompt(
    text: &str,
    max_length: usize,
    mode: SpeechPrepMode,
    strategy: SpeechPrepStrategy,
    tag_palette: &[String],
    context: &SpeechPrepContext<'_>,
) -> String {
    match mode {
        SpeechPrepMode::Shorten => format!(
            "Prepare this text for text-to-speech playback. Preserve the user's meaning, key facts, decisions, and the full requested message. Shorten only when necessary to stay under {max_length} characters. Keep the prepared text at least {min_length} characters unless the source text itself is shorter. Do not collapse prose into a short abstract. Remove repetition, code blocks, URLs, file paths, and formatting noise. Return only natural speakable prose, no markdown, no preamble, no labels.\n\nText:\n\"\"\"{text}\"\"\"",
            min_length = shorten_min_output_chars(text.chars().count(), max_length)
        ),
        SpeechPrepMode::PerformanceTags => match strategy {
            SpeechPrepStrategy::InlineTags => {
                build_performance_tags_prompt(text, max_length, tag_palette, context)
            }
            SpeechPrepStrategy::StyleInstruction => {
                build_style_instruction_prompt(text, STYLE_INSTRUCTION_MAX_CHARS, context)
            }
            SpeechPrepStrategy::Off => String::new(),
        },
    }
}

pub(super) fn build_performance_tags_prompt(
    text: &str,
    max_length: usize,
    tag_palette: &[String],
    context: &SpeechPrepContext<'_>,
) -> String {
    let mut prompt = String::with_capacity(text.len() + 1600);
    prompt.push_str("You are a TTS performance tagger. Do not rewrite the text. Do not summarize, omit, or reorder it. Build a coherent performance arc by inserting concise emotion or delivery tags at meaningful changes in emotional state, pacing, tension, realization, or physical performance. Choose the most textually supported cue: distinguish dread, shock, revulsion, grief, irony, urgency, and tenderness instead of substituting a generic mood. Do not invent sorrow, urgency, bitterness, humor, or physical reactions unless the words support them. Keep each cue local to the complete sentence or clause it governs. Place each tag immediately before that sentence or clause, never between a determiner and its noun or inside a fixed phrase. Follow every closing bracket with exactly one space. Never place tags back-to-back; combine compatible direction into one concise bracketed cue when necessary. Do not impose an arbitrary limit on the number of tags; cover the emotional progression throughout the text, but avoid redundant cues where delivery does not change. For emotionally charged prose longer than 800 characters, sustain cues through the final emotionally active sentence and use roughly one meaningful cue per 80-140 characters as coverage guidance, not as a minimum or maximum count. Do not stop tagging merely because the opening and climax have cues. Prefer performable direction over literary analysis. Never add a cue that contradicts the text. Return only the tagged text, with no enclosing quotation marks, code fence, label, or delimiter. Every cue must be enclosed in square brackets, like [softly] or [gasps, horrified]. If the text is genuinely neutral and no cue improves delivery, return it unchanged.\n");
    prompt.push_str("Semantic distinction: reserve sorrow and grief for actual loss, mourning, tears, or regret. Grotesque imagery, bodily horror, and fearful disgust call for dread, horror, or revulsion instead of sorrow.\n");
    prompt.push_str("Use inline bracketed audio tags from this palette when they fit: ");
    for (index, tag) in tag_palette.iter().enumerate() {
        if index > 0 {
            prompt.push_str(", ");
        }
        prompt.push('[');
        prompt.push_str(tag);
        prompt.push(']');
    }
    prompt.push_str(". Closely related performable cues are allowed when the palette does not fit, but they must also be square-bracketed. Keep the result under ");
    prompt.push_str(&max_length.to_string());
    prompt.push_str(" characters.\n\n");

    push_delivery_context(&mut prompt, context);

    prompt.push_str("Text:\n\"\"\"");
    prompt.push_str(text);
    prompt.push_str("\"\"\"");
    prompt
}

pub(super) fn build_style_instruction_prompt(
    text: &str,
    max_instruction_length: usize,
    context: &SpeechPrepContext<'_>,
) -> String {
    let mut prompt = String::with_capacity(text.len() + 1400);
    prompt.push_str("You are a TTS delivery director for Google Gemini speech synthesis. Do not rewrite, summarize, quote, or repeat the text. Return only a 1-3 sentence natural-language delivery instruction for how the voice should perform this exact message: emotional state, pacing, intimacy, tension, hesitation, warmth, and release. Keep it concrete and speakable as direction, not content. Never include bracket tags. Keep the instruction under ");
    prompt.push_str(&max_instruction_length.to_string());
    prompt.push_str(" characters.\n\n");
    push_delivery_context(&mut prompt, context);
    prompt.push_str("Text to direct, not rewrite:\n\"\"\"");
    prompt.push_str(text);
    prompt.push_str("\"\"\"");
    prompt
}

pub(super) fn push_delivery_context(prompt: &mut String, context: &SpeechPrepContext<'_>) {
    if let Some(persona) = context.persona {
        prompt.push_str("Delivery context:\n");
        prompt.push_str("- persona: ");
        prompt.push_str(&persona.label);
        prompt.push_str(" - ");
        prompt.push_str(&persona.description);
        prompt.push('\n');
        if let Some(scene) = &persona.prompt_scene {
            prompt.push_str("- scene: ");
            prompt.push_str(scene);
            prompt.push('\n');
        }
        if let Some(style) = &persona.prompt_style {
            prompt.push_str("- style: ");
            prompt.push_str(style);
            prompt.push('\n');
        }
        if let Some(pacing) = &persona.prompt_pacing {
            prompt.push_str("- pace: ");
            prompt.push_str(pacing);
            prompt.push('\n');
        }
        for constraint in &persona.prompt_constraints {
            prompt.push_str("- constraint: ");
            prompt.push_str(constraint);
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if let Some(instructions) = context.instructions {
        prompt.push_str("Additional delivery hints:\n");
        prompt.push_str(instructions);
        prompt.push_str("\n\n");
    }
}
