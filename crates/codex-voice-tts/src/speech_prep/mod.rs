mod prompts;
mod shorten;
mod tag_repair;

use codex_voice_core::{SpeechError, SpeechResult};
use reqwest::Client;

use prompts::build_prompt;
use shorten::{extractive_shorten_to_fit, shorten_or_extract};
use tag_repair::{repair_bare_leading_performance_cue, validate_performance_tags_output};

use crate::codex_llm::CodexLlmClient;
use crate::config::{
    ProviderKind, ResolvedPersona, SpeechPrepConfig, SpeechPrepMode, SpeechPrepProviderKind,
    SpeechPrepStrategy,
};
use crate::sanitize::sanitize_for_tts;

const PERFORMANCE_TAGS_DEFAULT_MAX_OUTPUT_TOKENS: usize = 384;
const PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS: usize = 4096;
const MIN_SHORTEN_OUTPUT_CHARS: usize = 4_000;
const STYLE_INSTRUCTION_MAX_CHARS: usize = 300;

pub struct SpeechPrepClient {
    config: SpeechPrepConfig,
    client: Client,
    codex: Option<CodexLlmClient>,
}

#[derive(Clone, Copy)]
pub struct SpeechPrepContext<'a> {
    pub target: SpeechPrepTarget<'a>,
    pub persona: Option<&'a ResolvedPersona>,
    pub instructions: Option<&'a str>,
}

#[derive(Clone, Copy)]
pub struct SpeechPrepTarget<'a> {
    pub provider: ProviderKind,
    pub model_id: &'a str,
    pub supports_inline_audio_tags: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechPrepOutput {
    Input(String),
    DeliveryInstruction(String),
}

impl SpeechPrepClient {
    pub fn new(config: SpeechPrepConfig) -> Result<Self, SpeechError> {
        let client = Client::builder().build().map_err(|e| {
            SpeechError::Request(format!("failed to build speech prep client: {e}"))
        })?;
        let codex = if config.provider == SpeechPrepProviderKind::Codex {
            Some(CodexLlmClient::new(
                config.auth_file.clone().ok_or_else(|| {
                    SpeechError::Config("Codex speech prep is missing auth file".into())
                })?,
                config.base_url.clone(),
                config.timeout,
            )?)
        } else {
            None
        };
        Ok(Self {
            config,
            client,
            codex,
        })
    }

    pub fn strategy_for_target(&self, target: &SpeechPrepTarget<'_>) -> SpeechPrepStrategy {
        if self.config.mode == SpeechPrepMode::Shorten {
            return SpeechPrepStrategy::InlineTags;
        }

        let configured = match target.provider {
            ProviderKind::Google => self.config.strategies.google,
            ProviderKind::ElevenLabs => self.config.strategies.elevenlabs,
        };
        let strategy = if configured == SpeechPrepStrategy::Off {
            self.config.strategies.default
        } else {
            configured
        };

        match strategy {
            SpeechPrepStrategy::InlineTags if target.supports_inline_audio_tags => {
                SpeechPrepStrategy::InlineTags
            }
            SpeechPrepStrategy::InlineTags => SpeechPrepStrategy::Off,
            SpeechPrepStrategy::StyleInstruction
                if target.provider == ProviderKind::Google
                    && google_model_supports_style_instruction(target.model_id) =>
            {
                SpeechPrepStrategy::StyleInstruction
            }
            SpeechPrepStrategy::StyleInstruction => SpeechPrepStrategy::Off,
            SpeechPrepStrategy::Off => SpeechPrepStrategy::Off,
        }
    }

    pub fn mode(&self) -> SpeechPrepMode {
        self.config.mode
    }

    pub fn should_prepare(&self, text: &str, target: &SpeechPrepTarget<'_>) -> bool {
        let chars = text.chars().count();
        match self.config.mode {
            SpeechPrepMode::Shorten => {
                chars > self.shorten_prepare_floor() && chars > self.config.max_length
            }
            SpeechPrepMode::PerformanceTags => {
                self.strategy_for_target(target) != SpeechPrepStrategy::Off
                    && chars >= self.config.threshold
                    && chars <= self.config.max_input_length
            }
        }
    }

    pub fn should_shorten_to_fit(&self, text: &str, max_length: usize) -> bool {
        text.chars().count() > max_length
    }

    pub fn extractive_shorten_to_fit(&self, text: &str, max_length: usize) -> SpeechResult<String> {
        let input = prepare_input_for_prompt(text, self.config.max_input_length)?;
        Ok(extractive_shorten_to_fit(&input, max_length))
    }

    pub fn fallback_performance_tags(
        &self,
        text: &str,
        target: &SpeechPrepTarget<'_>,
    ) -> SpeechResult<Option<String>> {
        if self.config.mode != SpeechPrepMode::PerformanceTags
            || self.strategy_for_target(target) != SpeechPrepStrategy::InlineTags
            || !target.supports_inline_audio_tags
            || !collect_bracket_tags(text).is_empty()
        {
            return Ok(None);
        }

        let insertions = sentence_ranges(text)
            .into_iter()
            .filter_map(|(start, end)| {
                fallback_performance_tag(&text[start..end], &self.config.tag_palette)
                    .map(|tag| (start, tag.to_string()))
            })
            .collect::<Vec<_>>();
        if insertions.is_empty() {
            return Ok(None);
        }
        let mut tagged = text.to_string();
        for (start, tag) in insertions.into_iter().rev() {
            tagged.insert_str(start, &format!("[{tag}] "));
        }
        let sanitized = sanitize_for_tts(&tagged, usize::MAX)?;
        if sanitized.chars().count() > self.config.max_length {
            return Ok(None);
        }
        validate_performance_tags_output(text, &sanitized)?;
        Ok(Some(sanitized))
    }

    pub async fn prepare_to_fit(
        &self,
        text: &str,
        context: SpeechPrepContext<'_>,
        max_length: usize,
    ) -> SpeechResult<Option<String>> {
        if !self.should_shorten_to_fit(text, max_length) {
            return Ok(None);
        }

        let input = prepare_input_for_prompt(text, self.config.max_input_length)?;
        let prompt = build_prompt(
            &input,
            max_length,
            SpeechPrepMode::Shorten,
            SpeechPrepStrategy::InlineTags,
            &self.config.tag_palette,
            &context,
        );
        let max_output_tokens = (max_length / 3).clamp(64, 4096);
        let body = match self.config.provider {
            SpeechPrepProviderKind::Google => Some(serde_json::json!({
                "contents": [
                    {
                        "role": "user",
                        "parts": [{ "text": prompt }]
                    }
                ],
                "generationConfig": {
                    "temperature": 0.2,
                    "maxOutputTokens": max_output_tokens
                }
            })),
            SpeechPrepProviderKind::Codex => None,
        };

        let prepared = self.prepare_with_fallbacks(&prompt, body.as_ref()).await?;
        let sanitized = sanitize_for_tts(&prepared, usize::MAX)?;
        let shortened = shorten_or_extract(&input, &sanitized, max_length);
        Ok(Some(shortened))
    }

    /// Run one performance-tags speech-prep generation for benchmarking.
    ///
    /// Builds the production performance-tags prompt for `text` and dispatches
    /// it to the configured provider, returning the raw model output. Unlike
    /// [`SpeechPrepClient::prepare`], this skips the `should_prepare` gating and
    /// the post-generation validation so a benchmark can measure latency and
    /// inspect output for any input. Provider HTTP is reused unchanged.
    pub async fn benchmark(&self, text: &str) -> SpeechResult<String> {
        let context = SpeechPrepContext {
            target: SpeechPrepTarget {
                provider: ProviderKind::Google,
                model_id: &self.config.model,
                supports_inline_audio_tags: true,
            },
            persona: None,
            instructions: None,
        };
        let input = prepare_input_for_prompt(text, self.config.max_input_length)?;
        let prompt = build_prompt(
            &input,
            self.config.max_length,
            SpeechPrepMode::PerformanceTags,
            SpeechPrepStrategy::InlineTags,
            &self.config.tag_palette,
            &context,
        );
        let max_output_tokens = performance_tags_max_output_tokens(
            input.chars().count(),
            self.config.max_length,
            self.config.cap_performance_tags,
        );
        let body = match self.config.provider {
            SpeechPrepProviderKind::Google => Some(serde_json::json!({
                "contents": [
                    {
                        "role": "user",
                        "parts": [{ "text": prompt }]
                    }
                ],
                "generationConfig": {
                    "temperature": 0.45,
                    "maxOutputTokens": max_output_tokens,
                    "thinkingConfig": { "thinkingLevel": "MINIMAL" }
                }
            })),
            SpeechPrepProviderKind::Codex => None,
        };
        self.prepare_with_fallbacks(&prompt, body.as_ref()).await
    }

    pub async fn prepare(
        &self,
        text: &str,
        context: SpeechPrepContext<'_>,
    ) -> SpeechResult<Option<SpeechPrepOutput>> {
        if !self.should_prepare(text, &context.target) {
            return Ok(None);
        }

        let strategy = self.strategy_for_target(&context.target);
        let input = prepare_input_for_prompt(text, self.config.max_input_length)?;
        let prompt = build_prompt(
            &input,
            self.config.max_length,
            self.config.mode,
            strategy,
            &self.config.tag_palette,
            &context,
        );
        let max_output_tokens = match (self.config.mode, strategy) {
            (SpeechPrepMode::Shorten, _) => (self.config.max_length / 3).clamp(64, 4096),
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::InlineTags) => {
                performance_tags_max_output_tokens(
                    input.chars().count(),
                    self.config.max_length,
                    self.config.cap_performance_tags,
                )
            }
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::StyleInstruction) => 128,
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::Off) => return Ok(None),
        };
        let mut generation_config = serde_json::json!({
            "temperature": match self.config.mode {
                SpeechPrepMode::Shorten => 0.2,
                SpeechPrepMode::PerformanceTags => 0.45,
            },
            "maxOutputTokens": max_output_tokens
        });
        if self.config.mode == SpeechPrepMode::PerformanceTags {
            generation_config["thinkingConfig"] = serde_json::json!({
                "thinkingLevel": "MINIMAL"
            });
        }
        let body = match self.config.provider {
            SpeechPrepProviderKind::Google => Some(serde_json::json!({
                "contents": [
                    {
                        "role": "user",
                        "parts": [{ "text": prompt }]
                    }
                ],
                "generationConfig": generation_config
            })),
            SpeechPrepProviderKind::Codex => None,
        };

        let prepared = self.prepare_with_fallbacks(&prompt, body.as_ref()).await?;
        let sanitized = sanitize_for_tts(&prepared, usize::MAX)?;
        let sanitized = match (self.config.mode, strategy) {
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::InlineTags) => {
                repair_bare_leading_performance_cue(&input, &sanitized, &self.config.tag_palette)
            }
            _ => sanitized,
        };
        let output = match (self.config.mode, strategy) {
            (SpeechPrepMode::Shorten, _) => {
                let shortened = shorten_or_extract(&input, &sanitized, self.config.max_length);
                SpeechPrepOutput::Input(shortened)
            }
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::InlineTags) => {
                if sanitized.chars().count() > self.config.max_length {
                    return Err(SpeechError::Request(format!(
                        "speech prep enrichment returned {} characters, above max {}",
                        sanitized.chars().count(),
                        self.config.max_length
                    )));
                }
                validate_performance_tags_output(&input, &sanitized)?;
                SpeechPrepOutput::Input(sanitized)
            }
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::StyleInstruction) => {
                validate_style_instruction_output(&input, &sanitized)?;
                SpeechPrepOutput::DeliveryInstruction(sanitized)
            }
            (SpeechPrepMode::PerformanceTags, SpeechPrepStrategy::Off) => return Ok(None),
        };

        let empty = match &output {
            SpeechPrepOutput::Input(value) | SpeechPrepOutput::DeliveryInstruction(value) => {
                value.trim().is_empty()
            }
        };
        if empty {
            return Err(SpeechError::Request(
                "speech prep returned empty text".into(),
            ));
        }

        Ok(Some(output))
    }

    fn shorten_prepare_floor(&self) -> usize {
        self.config
            .threshold
            .max(MIN_SHORTEN_OUTPUT_CHARS.min(self.config.max_length))
    }

    async fn prepare_with_fallbacks(
        &self,
        prompt: &str,
        body: Option<&serde_json::Value>,
    ) -> SpeechResult<String> {
        let started = std::time::Instant::now();
        let models = speech_prep_models(
            self.config.provider,
            &self.config.model,
            &self.config.fallback_models,
        );
        let mut last_retryable_error = None;
        for model in models {
            let elapsed = started.elapsed();
            if elapsed >= self.config.timeout {
                break;
            }
            let remaining = self.config.timeout - elapsed;
            let attempt_timeout = self.config.attempt_timeout.min(remaining);
            match self
                .prepare_once(&model, prompt, body, attempt_timeout)
                .await
            {
                Ok(prepared) => return Ok(prepared),
                Err(error) if speech_prep_error_is_retryable(&error) => {
                    last_retryable_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_retryable_error.unwrap_or_else(|| {
            SpeechError::Request(format!(
                "speech prep request timed out after {}s",
                self.config.timeout.as_secs()
            ))
        }))
    }

    async fn prepare_once(
        &self,
        model: &str,
        prompt: &str,
        body: Option<&serde_json::Value>,
        timeout: std::time::Duration,
    ) -> SpeechResult<String> {
        match self.config.provider {
            SpeechPrepProviderKind::Google => {
                let body = body.ok_or_else(|| {
                    SpeechError::Config("Google speech prep is missing request body".into())
                })?;
                self.prepare_google_once(model, body, timeout).await
            }
            SpeechPrepProviderKind::Codex => {
                let codex = self.codex.as_ref().ok_or_else(|| {
                    SpeechError::Config("Codex speech prep client is not configured".into())
                })?;
                codex
                    .generate_text(
                        model,
                        self.config.reasoning_effort.as_deref(),
                        prompt,
                        timeout,
                    )
                    .await
            }
        }
    }

    async fn prepare_google_once(
        &self,
        model: &str,
        body: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> SpeechResult<String> {
        let model = normalize_google_model_name(model);
        let url = format!("{}/models/{}:generateContent", self.config.base_url, model);
        let api_key =
            self.config.api_key.as_deref().ok_or_else(|| {
                SpeechError::Config("Google speech prep is missing API key".into())
            })?;
        tokio::time::timeout(timeout, async {
            let response = self
                .client
                .post(&url)
                .timeout(timeout)
                .header("x-goog-api-key", api_key)
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await
                .map_err(|e| SpeechError::Request(format!("speech prep request failed: {e}")))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(SpeechError::Service {
                    status: status.as_u16(),
                    message: format!("speech prep error: {text}"),
                });
            }

            let json: serde_json::Value = response.json().await.map_err(|e| {
                SpeechError::Request(format!("failed to parse speech prep response: {e}"))
            })?;
            extract_text(&json).ok_or_else(|| {
                SpeechError::Request("speech prep response missing text output".into())
            })
        })
        .await
        .map_err(|_| {
            SpeechError::Request(format!(
                "speech prep request timed out after {}s",
                timeout.as_secs()
            ))
        })?
    }
}

fn google_model_supports_style_instruction(model_id: &str) -> bool {
    let normalized = model_id
        .strip_prefix("google/")
        .unwrap_or(model_id)
        .to_ascii_lowercase();
    normalized.contains("gemini") && normalized.contains("tts")
}

fn speech_prep_error_is_retryable(error: &SpeechError) -> bool {
    match error {
        SpeechError::Service { status, .. } => *status == 429 || *status >= 500,
        SpeechError::Request(message) => {
            message.contains("timed out") || message.contains("request failed")
        }
        SpeechError::RateLimited(_) | SpeechError::Unavailable(_) => true,
        SpeechError::Message(_)
        | SpeechError::Config(_)
        | SpeechError::Auth(_)
        | SpeechError::Unsupported(_) => false,
    }
}

fn speech_prep_models(
    provider: SpeechPrepProviderKind,
    primary: &str,
    fallbacks: &[String],
) -> Vec<String> {
    let mut models: Vec<String> = Vec::with_capacity(fallbacks.len() + 1);
    for model in std::iter::once(primary).chain(fallbacks.iter().map(String::as_str)) {
        let normalized = normalize_speech_prep_model_name(provider, model);
        if models
            .iter()
            .any(|existing| normalize_speech_prep_model_name(provider, existing) == normalized)
        {
            continue;
        }
        models.push(model.to_string());
    }
    models
}

fn normalize_speech_prep_model_name(provider: SpeechPrepProviderKind, model: &str) -> &str {
    match provider {
        SpeechPrepProviderKind::Google => normalize_google_model_name(model),
        SpeechPrepProviderKind::Codex => model.strip_prefix("codex/").unwrap_or(model),
    }
}

fn prepare_input_for_prompt(text: &str, max_input_length: usize) -> SpeechResult<String> {
    let sanitized = sanitize_for_tts(text, usize::MAX)?;
    Ok(truncate_chars(&sanitized, max_input_length))
}

fn performance_tags_max_output_tokens(
    input_chars: usize,
    max_length: usize,
    cap_performance_tags: bool,
) -> usize {
    let max_default_tokens = if cap_performance_tags {
        PERFORMANCE_TAGS_DEFAULT_MAX_OUTPUT_TOKENS
    } else {
        PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS
    };
    let default_cap = (max_length / 2).clamp(128, max_default_tokens);
    let preserve_cap = (input_chars / 3).clamp(128, PERFORMANCE_TAGS_ABSOLUTE_MAX_OUTPUT_TOKENS);
    default_cap.max(preserve_cap)
}

fn shorten_min_output_chars(input_chars: usize, max_length: usize) -> usize {
    input_chars.min(max_length).min(MIN_SHORTEN_OUTPUT_CHARS)
}

fn fallback_performance_tag<'a>(text: &str, tag_palette: &'a [String]) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    let candidates = [
        (
            "whispers",
            ["whisper", "hushed", "under her breath", "under his breath"].as_slice(),
        ),
        (
            "sigh of relief",
            ["relief", "relieved", "finally breathe", "safe at last"].as_slice(),
        ),
        ("laughs", ["laugh", "laughed", "laughing"].as_slice()),
        (
            "light chuckle",
            ["smile", "smiled", "grin", "amused"].as_slice(),
        ),
        (
            "fearful",
            ["fear", "afraid", "terrified", "dread", "panic"].as_slice(),
        ),
        (
            "nervous",
            ["tremor", "trembling", "anxious", "nervous"].as_slice(),
        ),
        ("angry", ["angry", "furious", "rage", "outraged"].as_slice()),
        (
            "sorrowful",
            ["sorrow", "grief", "tears", "wept", "crying", "mourning"].as_slice(),
        ),
        (
            "wistful",
            ["remembered", "memory", "longed", "missed", "nostalgia"].as_slice(),
        ),
        (
            "frustrated",
            ["frustrated", "irritated", "annoyed", "stuck"].as_slice(),
        ),
        (
            "reassuring",
            ["safe", "steady", "promise", "trust", "breathe"].as_slice(),
        ),
        (
            "tender",
            [
                "tender",
                "gentle",
                "soft",
                "carefully",
                "held",
                "kiss",
                "kisses",
                "kissing",
                "lips",
                "leans over",
            ]
            .as_slice(),
        ),
        (
            "urgent",
            ["hurry", "urgent", "quickly", "now", "immediately"].as_slice(),
        ),
        (
            "breathless",
            ["breathless", "gasped", "panting", "ran"].as_slice(),
        ),
        (
            "proud",
            ["proud", "triumph", "victory", "accomplished"].as_slice(),
        ),
        (
            "excited",
            ["excited", "thrilled", "delighted", "eager"].as_slice(),
        ),
    ];

    candidates
        .iter()
        .find(|(tag, needles)| {
            tag_palette.iter().any(|palette_tag| palette_tag == tag)
                && needles.iter().any(|needle| lower.contains(needle))
        })
        .map(|(tag, _)| *tag)
}

fn sentence_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut after_boundary = true;
    for (index, ch) in text.char_indices() {
        if after_boundary && !ch.is_whitespace() {
            starts.push(index);
            after_boundary = false;
        }
        if matches!(ch, '.' | '!' | '?' | '\n') {
            after_boundary = true;
        }
    }
    let mut ranges = starts
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect::<Vec<_>>();
    if let Some(start) = starts.last().copied() {
        ranges.push((start, text.len()));
    }
    ranges
}

fn repair_leading_bare_cue(original: &str, prepared: &str, tag_palette: &[String]) -> String {
    let trimmed = prepared.trim_start();
    let leading_ws_len = prepared.len().saturating_sub(trimmed.len());
    let Some(source_start) = preserved_text_start(original, trimmed) else {
        return prepared.to_string();
    };
    if source_start == 0 {
        return prepared.to_string();
    }

    let cue = trimmed[..source_start].trim_matches(is_bare_cue_delimiter);
    if cue.is_empty() || !looks_like_bare_performance_cue(cue, tag_palette) {
        return prepared.to_string();
    }

    let rest = trimmed[source_start..].trim_start();
    if rest.is_empty() {
        return prepared.to_string();
    }

    let repaired = format!("{}[{cue}] {rest}", &prepared[..leading_ws_len]);
    if preservation_ratio(original, &repaired) >= 0.97 {
        repaired
    } else {
        prepared.to_string()
    }
}

fn preserved_text_start(original: &str, prepared: &str) -> Option<usize> {
    let original_words = words_without_tags(original);
    let first_words = original_words.iter().take(3).collect::<Vec<_>>();
    if first_words.is_empty() {
        return None;
    }

    let prepared_words = word_spans_without_tags(prepared);
    prepared_words
        .iter()
        .enumerate()
        .find(|(index, _)| {
            first_words.iter().enumerate().all(|(offset, expected)| {
                prepared_words
                    .get(index + offset)
                    .is_some_and(|(candidate, _, _)| candidate == *expected)
            })
        })
        .map(|(_, (_, start, _))| *start)
}

fn is_inside_bracket_tag(text: &str, index: usize) -> bool {
    let prefix = &text[..index];
    prefix
        .rfind('[')
        .is_some_and(|open| prefix[open..].find(']').is_none())
}

fn cue_trailing_delimiter_len(value: &str) -> Option<usize> {
    let mut len = 0;
    let mut saw_separator = false;
    for (index, ch) in value.char_indices() {
        if ch == ':' || ch == ',' || ch == '-' || ch == '.' || ch == '!' || ch == '?' {
            len = index + ch.len_utf8();
            saw_separator = true;
            continue;
        }
        if ch.is_whitespace() {
            len = index + ch.len_utf8();
            saw_separator = true;
            continue;
        }
        break;
    }
    saw_separator.then_some(len)
}

fn looks_like_bare_performance_cue(cue: &str, tag_palette: &[String]) -> bool {
    // Palette phrases are user-supplied and may contain non-ASCII letters
    // (e.g. "café"), so fold with full Unicode case rules rather than
    // ASCII-only lowercasing.
    let lower = cue.to_lowercase();
    let words = words_without_tags(cue);
    if words.is_empty() || words.len() > 5 {
        return false;
    }
    if tag_palette.iter().any(|tag| tag.to_lowercase() == lower) {
        return true;
    }

    const CUE_WORDS: &[&str] = &[
        "affectionate",
        "amused",
        "angry",
        "breathless",
        "calm",
        "chuckle",
        "chuckles",
        "deadpan",
        "dryly",
        "exhale",
        "exhales",
        "fearful",
        "flatly",
        "frustrated",
        "gasp",
        "gasps",
        "hesitates",
        "laugh",
        "laughing",
        "laughs",
        "leans",
        "lowers",
        "kiss",
        "kisses",
        "kissing",
        "lips",
        "moan",
        "moans",
        "nervous",
        "pause",
        "proud",
        "relieved",
        "reassuring",
        "scoffs",
        "serious",
        "shaky",
        "sigh",
        "sighs",
        "sleepy",
        "smile",
        "smiles",
        "smiling",
        "softly",
        "sorrowful",
        "swallows",
        "tender",
        "teasing",
        "urgent",
        "vulnerable",
        "warmly",
        "whisper",
        "whispers",
        "wistful",
    ];
    words
        .iter()
        .any(|word| CUE_WORDS.iter().any(|cue_word| word == cue_word))
}

fn bare_performance_cue_phrases(tag_palette: &[String]) -> Vec<String> {
    let mut phrases = vec![
        "smiles softly",
        "smiles and lowers my voice",
        "smiles and lowers her voice",
        "smiles and lowers his voice",
        "smiles and lowers their voice",
        "lowers my voice",
        "lowers her voice",
        "lowers his voice",
        "lowers their voice",
        "leans over and kisses your lips softly",
        "leans over and kisses her lips softly",
        "leans over and kisses his lips softly",
        "leans over and kisses their lips softly",
        "leans over and kisses you softly",
        "leans over and kisses her softly",
        "leans over and kisses him softly",
        "leans over and kisses them softly",
        "laughs softly",
        "chuckles softly",
        "sighs softly",
        "whispers softly",
        "smiles",
        "smiling",
        "laughs",
        "laughing",
        "chuckles",
        "sighs",
        "sigh",
        "whispers",
        "gasps",
        "exhales",
        "moans",
        "hesitates",
        "swallows",
        "voice breaks",
        "leans closer",
        "under breath",
        "softly",
        "warmly",
        "dryly",
        "flatly",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    phrases.extend(
        tag_palette
            .iter()
            .filter(|tag| looks_like_bare_performance_cue(tag, tag_palette))
            .map(|tag| tag.to_lowercase()),
    );
    phrases.sort_by_key(|phrase| std::cmp::Reverse(phrase.len()));
    phrases.dedup();
    phrases
}

fn is_bare_cue_delimiter(ch: char) -> bool {
    ch == ':' || ch == ',' || ch == '-' || ch == '.' || ch == '!' || ch == '?' || ch.is_whitespace()
}

fn strip_ascii_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix_len = prefix.len();
    // A byte index that lands inside a multibyte char would panic to slice —
    // and cannot match an ASCII prefix anyway, so treat it as a non-match.
    if value.len() < prefix_len || !value.is_char_boundary(prefix_len) {
        return None;
    }
    let candidate = &value[..prefix_len];
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(&value[prefix_len..])
    } else {
        None
    }
}

/// Unicode-aware case-insensitive prefix strip.
///
/// User-supplied palette phrases (e.g. from `codex-voice/config.json`) can
/// contain non-ASCII letters, so a plain byte-length prefix strip is not
/// safe: `char::to_lowercase()` can change the byte (and even char) length
/// of a folded string (e.g. Turkish `İ` folds to the two-character `i̇`).
/// This walks both strings char-by-char, comparing folded chars, and only
/// ever returns a slice at a boundary derived from `value.char_indices()`,
/// so the result is always char-boundary-safe.
fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(value);
    }
    if prefix.is_ascii() {
        // Fast path: byte-length prefix stripping is safe when the prefix is
        // pure ASCII, since ASCII case folding never changes byte length.
        return strip_ascii_prefix_ignore_case(value, prefix);
    }

    let prefix_folded: Vec<char> = prefix.chars().flat_map(char::to_lowercase).collect();
    let mut matched = 0usize;
    for (index, ch) in value.char_indices() {
        let end = index + ch.len_utf8();
        for folded in ch.to_lowercase() {
            if matched >= prefix_folded.len() || prefix_folded[matched] != folded {
                return None;
            }
            matched += 1;
        }
        if matched == prefix_folded.len() {
            return Some(&value[end..]);
        }
    }
    None
}

fn validate_style_instruction_output(original: &str, instruction: &str) -> SpeechResult<()> {
    let trimmed = instruction.trim();
    if trimmed.chars().count() > STYLE_INSTRUCTION_MAX_CHARS {
        return Err(SpeechError::Request(format!(
            "speech prep delivery instruction returned {} characters, above max {}",
            trimmed.chars().count(),
            STYLE_INSTRUCTION_MAX_CHARS
        )));
    }
    if trimmed.contains('[') || trimmed.contains(']') {
        return Err(SpeechError::Request(
            "speech prep delivery instruction contained bracket tags".into(),
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("delivery instruction:")
        || lower.starts_with("instruction:")
        || lower.starts_with("here")
        || lower.contains("```")
    {
        return Err(SpeechError::Request(
            "speech prep delivery instruction included formatting or preamble".into(),
        ));
    }
    let original_words = words_without_tags(original);
    let instruction_words = words_without_tags(trimmed);
    if original_words.len() >= 8 && preservation_ratio(original, trimmed) > 0.45 {
        return Err(SpeechError::Request(
            "speech prep delivery instruction repeated too much source text".into(),
        ));
    }
    if instruction_words.len() < 3 {
        return Err(SpeechError::Request(
            "speech prep delivery instruction was too short".into(),
        ));
    }
    Ok(())
}

/// Collect square-bracket audio/performance tags (e.g. `[softly]`) from `text`.
///
/// Tags must be non-empty, at most 80 bytes, and single-line; other bracket
/// content is skipped. Shared by speech-prep validation and the CLI benchmark
/// so both count tags identically.
pub fn collect_bracket_tags(text: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find(']') else {
            break;
        };
        let tag = &rest[..end];
        if !tag.is_empty() && tag.len() <= 80 && !tag.contains('\n') {
            tags.push(tag);
        }
        rest = &rest[end + 1..];
    }
    tags
}

fn words_without_tags(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut in_tag = false;
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            '[' if current.is_empty() => {
                in_tag = true;
            }
            ']' if in_tag => {
                in_tag = false;
            }
            _ if in_tag => {}
            _ if ch.is_alphanumeric() || ch == '\'' => {
                current.extend(ch.to_lowercase());
            }
            _ if !current.is_empty() => {
                words.push(std::mem::take(&mut current));
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn word_spans_without_tags(text: &str) -> Vec<(String, usize, usize)> {
    let mut words = Vec::new();
    let mut in_tag = false;
    let mut current = String::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        match ch {
            '[' if current.is_empty() => {
                in_tag = true;
            }
            ']' if in_tag => {
                in_tag = false;
            }
            _ if in_tag => {}
            _ if ch.is_alphanumeric() || ch == '\'' => {
                if current.is_empty() {
                    start = index;
                }
                current.extend(ch.to_lowercase());
            }
            _ if !current.is_empty() => {
                words.push((std::mem::take(&mut current), start, index));
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        words.push((current, start, text.len()));
    }
    words
}

fn preservation_ratio(original: &str, prepared: &str) -> f64 {
    let original_words = words_without_tags(original);
    if original_words.is_empty() {
        return 1.0;
    }
    let prepared_words = words_without_tags(prepared);
    let mut found = 0_usize;
    let mut cursor = 0_usize;
    for word in &original_words {
        while cursor < prepared_words.len() && prepared_words[cursor] != *word {
            cursor += 1;
        }
        if cursor >= prepared_words.len() {
            continue;
        }
        found += 1;
        cursor += 1;
    }
    found as f64 / original_words.len() as f64
}

fn extract_text(json: &serde_json::Value) -> Option<String> {
    let parts = json
        .get("candidates")?
        .as_array()?
        .first()?
        .get("content")?
        .get("parts")?
        .as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn normalize_google_model_name(model: &str) -> &str {
    model.strip_prefix("google/").unwrap_or(model)
}

fn truncate_chars(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        return text.to_string();
    }
    let limit = max_length.saturating_sub(1);
    let mut truncated = text.chars().take(limit).collect::<String>();
    truncated.truncate(truncated.trim_end().len());
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::shorten::validate_shorten_output;
    use super::*;

    #[test]
    fn strip_ascii_prefix_ignores_multibyte_boundary_instead_of_panicking() {
        // Regression: a prefix whose byte length lands inside a multibyte char
        // (e.g. the '\u{2026}' ellipsis in real prose) used to panic on the
        // byte slice. It must simply be a non-match.
        assert_eq!(
            strip_ascii_prefix_ignore_case("ab\u{2026}cdef", "abc"),
            None
        );
        // Matching still works and is case-insensitive.
        assert_eq!(
            strip_ascii_prefix_ignore_case("Tender words", "tender"),
            Some(" words")
        );
        // Shorter values are still a clean non-match.
        assert_eq!(strip_ascii_prefix_ignore_case("ab", "abc"), None);
    }

    #[test]
    fn strip_prefix_ignore_case_matches_non_ascii_palette_phrases() {
        // Non-ASCII palette phrases (e.g. user-configured "café") must
        // case-fold correctly rather than silently under-matching.
        assert_eq!(
            strip_prefix_ignore_case("Café au lait", "café"),
            Some(" au lait")
        );
        assert_eq!(
            strip_prefix_ignore_case("CAFÉ au lait", "café"),
            Some(" au lait")
        );
        assert_eq!(strip_prefix_ignore_case("Latte au lait", "café"), None);
    }

    #[test]
    fn strip_prefix_ignore_case_handles_folding_that_changes_byte_length() {
        // Latin capital letter sharp S (U+1E9E, 3 bytes in UTF-8) lowercases
        // to sharp S (U+00DF, 2 bytes in UTF-8), so the matched prefix
        // length in the value differs from the phrase's byte length. This
        // must not panic and must not mis-slice at a non-boundary.
        let value = "\u{1E9E} nights";
        let result = strip_prefix_ignore_case(value, "\u{00DF}");
        assert_eq!(result, Some(" nights"));
    }

    #[test]
    fn strip_prefix_ignore_case_ascii_behavior_is_unchanged() {
        assert_eq!(strip_prefix_ignore_case("ab\u{2026}cdef", "abc"), None);
        assert_eq!(
            strip_prefix_ignore_case("Tender words", "tender"),
            Some(" words")
        );
        assert_eq!(strip_prefix_ignore_case("ab", "abc"), None);
    }

    fn default_test_palette() -> Vec<String> {
        vec![
            "tender".to_string(),
            "softly".to_string(),
            "amused".to_string(),
            "nervous".to_string(),
            "sigh of relief".to_string(),
        ]
    }

    fn test_target(
        provider: crate::config::ProviderKind,
        model_id: &'static str,
        supports_inline_audio_tags: bool,
    ) -> SpeechPrepTarget<'static> {
        SpeechPrepTarget {
            provider,
            model_id,
            supports_inline_audio_tags,
        }
    }

    #[test]
    fn strips_provider_prefix_from_google_model() {
        assert_eq!(
            normalize_google_model_name("google/gemini-3.5-flash"),
            "gemini-3.5-flash"
        );
        assert_eq!(
            normalize_google_model_name("gemini-3.5-flash"),
            "gemini-3.5-flash"
        );
    }

    #[test]
    fn truncates_on_character_boundary() {
        assert_eq!(truncate_chars("hello world", 20), "hello world");
        assert_eq!(truncate_chars("hello world", 6), "hello…");
    }

    #[test]
    fn extracts_text_parts_from_response() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "short"}, {"text": "summary"}]
                }
            }]
        });
        assert_eq!(extract_text(&json).unwrap(), "short summary");
    }

    #[test]
    fn speech_prep_config_keeps_input_cap_at_or_above_threshold() {
        let config = SpeechPrepConfig {
            provider: crate::config::SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::Shorten,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: default_test_palette(),
            cap_performance_tags: false,
            threshold: 700,
            max_input_length: 700,
            max_length: 420,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        let target = test_target(
            crate::config::ProviderKind::Google,
            "gemini-3.5-flash",
            false,
        );
        assert!(client.should_prepare(&"x".repeat(701), &target));
    }

    #[test]
    fn speech_prep_skips_text_that_already_fits_output_limit() {
        let config = SpeechPrepConfig {
            provider: crate::config::SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::Shorten,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: default_test_palette(),
            cap_performance_tags: false,
            threshold: 500,
            max_input_length: 12_000,
            max_length: 3000,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        let target = test_target(
            crate::config::ProviderKind::Google,
            "gemini-3.5-flash",
            false,
        );
        assert!(!client.should_prepare(&"x".repeat(700), &target));
    }

    #[test]
    fn shorten_mode_skips_inputs_below_4000_when_provider_allows_it() {
        let config = SpeechPrepConfig {
            provider: crate::config::SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::Shorten,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: default_test_palette(),
            cap_performance_tags: false,
            threshold: 500,
            max_input_length: 12_000,
            max_length: 4000,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        let target = test_target(
            crate::config::ProviderKind::Google,
            "gemini-3.5-flash",
            false,
        );
        assert!(!client.should_prepare(&"x".repeat(3999), &target));
        assert!(client.should_prepare(&"x".repeat(4001), &target));
    }

    #[test]
    fn shorten_prompt_and_validation_enforce_minimum_output_floor() {
        let context = SpeechPrepContext {
            target: test_target(
                crate::config::ProviderKind::Google,
                "gemini-3.5-flash",
                false,
            ),
            persona: None,
            instructions: None,
        };

        let prompt = build_prompt(
            &"a".repeat(6000),
            4000,
            SpeechPrepMode::Shorten,
            SpeechPrepStrategy::InlineTags,
            &default_test_palette(),
            &context,
        );

        assert!(prompt.contains("at least 4000 characters"));
        assert!(prompt.contains("complete semantic meaning"));
        assert!(prompt.contains("author's voice and point of view"));
        assert!(prompt.contains("distinctive imagery"));
        assert!(prompt.contains("Do not sanitize, moralize, euphemize"));
        assert!(prompt.contains("make surgical cuts"));
        assert!(prompt.contains("Do not add bracketed performance tags"));
        validate_shorten_output(6000, &"a".repeat(3999), 4000).unwrap_err();
        validate_shorten_output(6000, &"a".repeat(4000), 4000).unwrap();
    }

    #[test]
    fn shorten_falls_back_to_source_excerpt_when_model_collapses_text() {
        let input = "source ".repeat(1000);
        let output = shorten_or_extract(&input, "tiny summary", 5000);

        assert!(output.chars().count() >= 4000);
        assert!(output.chars().count() <= 5000);
        assert!(output.starts_with("source source"));
    }

    #[test]
    fn performance_tag_token_budget_scales_for_long_text() {
        assert_eq!(performance_tags_max_output_tokens(225, 6000, true), 384);
        assert_eq!(performance_tags_max_output_tokens(225, 6000, false), 3000);
        assert_eq!(performance_tags_max_output_tokens(5400, 6000, true), 1800);
        assert_eq!(performance_tags_max_output_tokens(5400, 6000, false), 3000);
    }

    #[test]
    fn speech_prep_input_is_sanitized_and_truncated_without_rejection() {
        let text = format!("{} ```ignored code```", "word ".repeat(100));

        let result = prepare_input_for_prompt(&text, 40).unwrap();

        assert!(result.chars().count() <= 40);
        assert!(!result.contains("```"));
        assert!(result.ends_with('…'));
    }

    #[test]
    fn performance_tags_only_prepare_when_model_supports_tags() {
        let config = SpeechPrepConfig {
            provider: crate::config::SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::PerformanceTags,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: default_test_palette(),
            cap_performance_tags: false,
            threshold: 120,
            max_input_length: 12_000,
            max_length: 3000,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        let elevenlabs_v3 = test_target(crate::config::ProviderKind::ElevenLabs, "eleven_v3", true);
        let elevenlabs_flash = test_target(
            crate::config::ProviderKind::ElevenLabs,
            "eleven_flash_v2_5",
            false,
        );
        let google_tts = test_target(
            crate::config::ProviderKind::Google,
            "gemini-3.1-flash-tts-preview",
            true,
        );

        assert!(client.should_prepare(&"I did it. ".repeat(20), &elevenlabs_v3));
        assert!(!client.should_prepare(&"I did it. ".repeat(20), &elevenlabs_flash));
        assert!(client.should_prepare(&"I did it. ".repeat(20), &google_tts));
    }

    #[test]
    fn performance_tags_prompt_forbids_summarization() {
        let context = SpeechPrepContext {
            target: test_target(crate::config::ProviderKind::ElevenLabs, "eleven_v3", true),
            persona: None,
            instructions: Some("Keep it warm."),
        };

        let prompt = build_prompt(
            "I was worried, but it worked.",
            1000,
            SpeechPrepMode::PerformanceTags,
            SpeechPrepStrategy::InlineTags,
            &default_test_palette(),
            &context,
        );

        assert!(prompt.contains("Do not summarize"));
        assert!(prompt.contains("Do not rewrite the text"));
        assert!(prompt.contains("Every cue must be enclosed in square brackets"));
        assert!(prompt.contains("[sigh of relief]"));
        assert!(prompt.contains("Return only the tagged text"));
        assert!(prompt.contains("Do not impose an arbitrary limit on the number of tags"));
        assert!(prompt.contains("sustain cues through the final emotionally active sentence"));
        assert!(prompt.contains("coverage guidance, not as a minimum or maximum count"));
        assert!(prompt.contains("no enclosing quotation marks, code fence, label, or delimiter"));
        assert!(prompt.contains("reserve sorrow and grief for actual loss"));
        assert!(!prompt.contains("Use tags sparingly"));
        assert!(prompt.contains("Follow every closing bracket with exactly one space"));
        assert!(prompt.contains("Never place tags back-to-back"));
        assert!(prompt.contains("never between a determiner and its noun"));
    }

    #[test]
    fn fallback_performance_tags_adds_context_local_cues_for_each_transition() {
        let config = SpeechPrepConfig {
            provider: SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::PerformanceTags,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: vec![
                "fearful".to_string(),
                "sigh of relief".to_string(),
                "laughs".to_string(),
                "proud".to_string(),
            ],
            cap_performance_tags: false,
            threshold: 120,
            max_input_length: 12_000,
            max_length: 6000,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();
        let target = test_target(crate::config::ProviderKind::ElevenLabs, "eleven_v3", true);
        let input = "Mara felt a tremor and feared the worst. Then she smiled, finally safe at last. They laughed and celebrated the victory.";

        let tagged = client
            .fallback_performance_tags(input, &target)
            .unwrap()
            .unwrap();

        assert_eq!(
            tagged,
            "[fearful] Mara felt a tremor and feared the worst. [sigh of relief] Then she smiled, finally safe at last. [laughs] They laughed and celebrated the victory."
        );
        validate_performance_tags_output(input, &tagged).unwrap();
    }

    #[test]
    fn fallback_performance_tags_marks_intimate_action_tender() {
        let config = SpeechPrepConfig {
            provider: SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::PerformanceTags,
            api_key: Some("key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: crate::config::SpeechPrepStrategies::default(),
            tag_palette: default_test_palette(),
            cap_performance_tags: false,
            threshold: 120,
            max_input_length: 12_000,
            max_length: 6000,
            attempt_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();
        let target = test_target(crate::config::ProviderKind::ElevenLabs, "eleven_v3", true);
        let input = "Leans over and kisses your lips softly before saying goodnight.";

        let tagged = client
            .fallback_performance_tags(input, &target)
            .unwrap()
            .unwrap();

        assert_eq!(
            tagged,
            "[tender] Leans over and kisses your lips softly before saying goodnight."
        );
        validate_performance_tags_output(input, &tagged).unwrap();
    }

    #[test]
    fn style_instruction_prompt_keeps_text_out_of_output() {
        let context = SpeechPrepContext {
            target: test_target(
                crate::config::ProviderKind::Google,
                "gemini-2.5-flash-preview-tts",
                false,
            ),
            persona: None,
            instructions: Some("Keep it warm."),
        };

        let prompt = build_prompt(
            "I was worried, but it worked.",
            1000,
            SpeechPrepMode::PerformanceTags,
            SpeechPrepStrategy::StyleInstruction,
            &default_test_palette(),
            &context,
        );

        assert!(prompt.contains("Do not rewrite"));
        assert!(prompt.contains("Never include bracket tags"));
        assert!(prompt.contains("Return only a 1-3 sentence"));
    }

    #[test]
    fn performance_tag_validation_accepts_sparse_tags_preserving_text() {
        validate_performance_tags_output(
            "I was worried, but it worked. Thank you for staying with me.",
            "[softly] I was worried, but it worked. Thank you for staying with me.",
        )
        .unwrap();
    }

    #[test]
    fn performance_tag_validation_accepts_two_sparse_tags_on_short_text() {
        validate_performance_tags_output(
            "I was worried, but it worked. Thank you for staying with me.",
            "[nervous] I was worried, but it worked. [sigh of relief] Thank you for staying with me.",
        )
        .unwrap();
    }

    #[test]
    fn performance_tag_validation_rejects_changed_output_without_brackets() {
        let error = validate_performance_tags_output(
            "I was worried, but it worked. Thank you for staying with me.",
            "softly I was worried, but it worked. Thank you for staying with me.",
        )
        .unwrap_err();

        assert!(error.to_string().contains("without square-bracket tags"));
    }

    #[test]
    fn repairs_bare_leading_palette_cue_to_bracketed_tag() {
        let input = "I was worried, but it worked. Thank you for staying with me.";
        let repaired = repair_bare_leading_performance_cue(
            input,
            "softly, I was worried, but it worked. Thank you for staying with me.",
            &default_test_palette(),
        );

        assert_eq!(
            repaired,
            "[softly] I was worried, but it worked. Thank you for staying with me."
        );
        validate_performance_tags_output(input, &repaired).unwrap();
    }

    #[test]
    fn repairs_bare_leading_cue_phrase_to_bracketed_tag() {
        let input = "I was worried, but it worked. Thank you for staying with me.";
        let repaired = repair_bare_leading_performance_cue(
            input,
            "Smiles softly, I was worried, but it worked. Thank you for staying with me.",
            &default_test_palette(),
        );

        assert_eq!(
            repaired,
            "[Smiles softly] I was worried, but it worked. Thank you for staying with me."
        );
        validate_performance_tags_output(input, &repaired).unwrap();
    }

    #[test]
    fn repairs_lowered_voice_cue_phrase_to_bracketed_tag() {
        let input = "I was worried, but it worked. Thank you for staying with me.";
        let repaired = repair_bare_leading_performance_cue(
            input,
            "Smiles and lowers my voice, I was worried, but it worked. Thank you for staying with me.",
            &default_test_palette(),
        );

        assert_eq!(
            repaired,
            "[Smiles and lowers my voice] I was worried, but it worked. Thank you for staying with me."
        );
        validate_performance_tags_output(input, &repaired).unwrap();
    }

    #[test]
    fn repairs_sentence_boundary_cue_phrase_to_bracketed_tag() {
        let input = "I was worried, but it worked. Thank you for staying with me.";
        let repaired = repair_bare_leading_performance_cue(
            input,
            "[nervous] I was worried, but it worked. Smiles softly, Thank you for staying with me.",
            &default_test_palette(),
        );

        assert_eq!(
            repaired,
            "[nervous] I was worried, but it worked. [smiles softly] Thank you for staying with me."
        );
        validate_performance_tags_output(input, &repaired).unwrap();
    }

    #[test]
    fn performance_tag_validation_rejects_rewrites_or_truncation() {
        let error = validate_performance_tags_output(
            "I was worried, but it worked. Thank you for staying with me.",
            "[softly] I was worried, but it worked.",
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed text too much"));
    }

    #[test]
    fn performance_tag_validation_rejects_non_latin_rewrite() {
        let error = validate_performance_tags_output("Привет мир", "[excited] Совсем другой текст")
            .unwrap_err();

        assert!(error.to_string().contains("changed text too much"));
    }

    #[test]
    fn performance_tag_validation_has_no_arbitrary_tag_count_limit() {
        let original = "word ".repeat(120);
        let prepared = format!("[softly] {} [tender] [whispers] [sigh] [laughs]", original);

        validate_performance_tags_output(&original, &prepared).unwrap();
    }
}
