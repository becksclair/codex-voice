use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use codex_voice_core::{SpeechError, SpeechResult};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_REFRESH_SKEW_SECONDS: u64 = 300;

#[derive(Debug, Clone)]
pub struct CodexLlmClient {
    client: Client,
    auth_file: PathBuf,
    base_url: String,
}

impl CodexLlmClient {
    pub fn new(auth_file: PathBuf, base_url: String, _timeout: Duration) -> SpeechResult<Self> {
        let client = Client::builder().build().map_err(|error| {
            SpeechError::Request(format!("failed to build Codex LLM client: {error}"))
        })?;
        Ok(Self {
            client,
            auth_file,
            base_url: normalize_codex_base_url(&base_url),
        })
    }

    pub async fn generate_text(
        &self,
        model: &str,
        reasoning_effort: Option<&str>,
        prompt: &str,
        timeout: Duration,
    ) -> SpeechResult<String> {
        let body = codex_responses_body(model, reasoning_effort, prompt);
        let started = std::time::Instant::now();
        let mut force_refresh = false;
        for _ in 0..2 {
            let remaining = timeout
                .checked_sub(started.elapsed())
                .unwrap_or_else(|| Duration::from_millis(1));
            let tokens = tokio::time::timeout(remaining, self.tokens(force_refresh))
                .await
                .map_err(|_| {
                    SpeechError::Request(format!(
                        "Codex speech prep request timed out after {}s",
                        timeout.as_secs()
                    ))
                })??;
            let remaining = timeout
                .checked_sub(started.elapsed())
                .unwrap_or_else(|| Duration::from_millis(1));
            let response = tokio::time::timeout(remaining, async {
                self.client
                    .post(format!("{}/responses", self.base_url))
                    .bearer_auth(&tokens.access_token)
                    .header("chatgpt-account-id", &tokens.account_id)
                    .header("originator", "codex-voice")
                    .header("User-Agent", "codex-voice")
                    .header("OpenAI-Beta", "responses=experimental")
                    .header("Accept", "text/event-stream")
                    .json(&body)
                    .send()
                    .await
            })
            .await
            .map_err(|_| {
                SpeechError::Request(format!(
                    "Codex speech prep request timed out after {}s",
                    remaining.as_secs()
                ))
            })?
            .map_err(|error| {
                SpeechError::Request(format!("Codex speech prep request failed: {error}"))
            })?;

            if matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) && !force_refresh
            {
                force_refresh = true;
                continue;
            }
            let status = response.status();
            if !status.is_success() {
                let remaining = timeout
                    .checked_sub(started.elapsed())
                    .unwrap_or_else(|| Duration::from_millis(1));
                let text = tokio::time::timeout(remaining, response.text())
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                return Err(SpeechError::Service {
                    status: status.as_u16(),
                    message: format!("Codex speech prep error: {text}"),
                });
            }

            let remaining = timeout
                .checked_sub(started.elapsed())
                .unwrap_or_else(|| Duration::from_millis(1));
            let text = tokio::time::timeout(remaining, response.text())
                .await
                .map_err(|_| {
                    SpeechError::Request(format!(
                        "Codex speech prep request timed out after {}s",
                        timeout.as_secs()
                    ))
                })?
                .map_err(|error| {
                    SpeechError::Request(format!(
                        "failed to read Codex speech prep response: {error}"
                    ))
                })?;
            let payload = parse_codex_sse(&text)?;
            return extract_codex_text(&payload).ok_or_else(|| {
                SpeechError::Request("Codex speech prep response missing text output".into())
            });
        }
        Err(SpeechError::Auth(
            "Codex auth refresh did not produce usable credentials".into(),
        ))
    }

    async fn tokens(&self, force_refresh: bool) -> SpeechResult<CodexAuthTokens> {
        let payload = read_auth_file(&self.auth_file)?;
        let tokens = extract_tokens(&payload)?;
        if !force_refresh && !access_token_needs_refresh(&tokens.access_token) {
            return Ok(tokens);
        }
        let refreshed = refresh_tokens(&self.client, &payload, &tokens.refresh_token).await?;
        write_auth_file(&self.auth_file, &refreshed)?;
        extract_tokens(&refreshed)
    }
}

#[derive(Debug, Clone)]
struct CodexAuthTokens {
    access_token: String,
    refresh_token: String,
    account_id: String,
}

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    tokens: CodexAuthFileTokens,
}

#[derive(Debug, Deserialize)]
struct CodexAuthFileTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

fn read_auth_file(path: &Path) -> SpeechResult<Value> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        SpeechError::Auth(format!(
            "failed to read Codex auth file {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&text)
        .map_err(|error| SpeechError::Auth(format!("failed to parse Codex auth file: {error}")))
}

fn extract_tokens(payload: &Value) -> SpeechResult<CodexAuthTokens> {
    let auth: CodexAuthFile = serde_json::from_value(payload.clone()).map_err(|error| {
        SpeechError::Auth(format!(
            "Codex auth file is missing required token fields: {error}"
        ))
    })?;
    let access_token = auth
        .tokens
        .access_token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| SpeechError::Auth("Codex auth file has no access token".into()))?;
    let refresh_token = auth
        .tokens
        .refresh_token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| SpeechError::Auth("Codex auth file has no refresh token".into()))?;
    let account_id = auth
        .tokens
        .account_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| SpeechError::Auth("Codex auth file has no account id".into()))?;
    Ok(CodexAuthTokens {
        access_token,
        refresh_token,
        account_id,
    })
}

async fn refresh_tokens(
    client: &Client,
    payload: &Value,
    refresh_token: &str,
) -> SpeechResult<Value> {
    let response = client
        .post(CODEX_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|error| SpeechError::Auth(format!("Codex auth refresh failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(SpeechError::Auth(format!(
            "Codex auth refresh failed with HTTP {}",
            status.as_u16()
        )));
    }
    let refreshed: Value = response.json().await.map_err(|error| {
        SpeechError::Auth(format!("Codex auth refresh returned invalid JSON: {error}"))
    })?;
    let access_token = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SpeechError::Auth("Codex auth refresh returned no access token".into()))?;
    if access_token_needs_refresh(access_token) {
        return Err(SpeechError::Auth(
            "Codex auth refresh returned an expired access token".into(),
        ));
    }

    let mut merged = payload.clone();
    if !merged.is_object() {
        merged = json!({});
    }
    let tokens = merged
        .as_object_mut()
        .expect("object checked above")
        .entry("tokens")
        .or_insert_with(|| json!({}));
    if !tokens.is_object() {
        *tokens = json!({});
    }
    let token_object = tokens.as_object_mut().expect("object checked above");
    for key in ["access_token", "refresh_token", "id_token", "account_id"] {
        if let Some(value) = refreshed.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                token_object.insert(key.to_string(), Value::String(value.to_string()));
            }
        }
    }
    merged
        .as_object_mut()
        .expect("object checked above")
        .insert(
            "last_refresh".to_string(),
            Value::String(format!("{}", chrono_like_timestamp())),
        );
    Ok(merged)
}

fn write_auth_file(path: &Path, payload: &Value) -> SpeechResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            SpeechError::Auth(format!("failed to create Codex auth directory: {error}"))
        })?;
    }
    let tmp_path = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("auth.json"),
        std::process::id()
    ));
    let text = serde_json::to_string(payload)
        .map_err(|error| SpeechError::Auth(format!("failed to encode Codex auth file: {error}")))?;
    std::fs::write(&tmp_path, format!("{text}\n")).map_err(|error| {
        SpeechError::Auth(format!(
            "failed to write refreshed Codex auth file: {error}"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp_path, path).map_err(|error| {
        SpeechError::Auth(format!(
            "failed to replace refreshed Codex auth file: {error}"
        ))
    })?;
    Ok(())
}

fn access_token_needs_refresh(access_token: &str) -> bool {
    let Some(payload) = access_token.split('.').nth(1) else {
        return true;
    };
    let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return true;
    };
    let Ok(json) = serde_json::from_slice::<Value>(&decoded) else {
        return true;
    };
    let Some(exp) = json.get("exp").and_then(Value::as_u64) else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    exp <= now + CODEX_REFRESH_SKEW_SECONDS
}

fn codex_responses_body(model: &str, reasoning_effort: Option<&str>, prompt: &str) -> Value {
    let model = normalize_codex_model(model);
    let mut body = json!({
        "model": model,
        "store": false,
        "stream": true,
        "instructions": "You are running non-interactively as a text transformation task. Do not use tools. Do not ask questions. Return only the transformed text.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt }]
        }],
        "text": { "verbosity": "low" },
        "parallel_tool_calls": false
    });
    if let Some(effort) = reasoning_effort.filter(|effort| !effort.eq_ignore_ascii_case("none")) {
        body["reasoning"] = json!({ "effort": effort });
    }
    body
}

fn normalize_codex_model(model: &str) -> &str {
    model.strip_prefix("codex/").unwrap_or(model)
}

fn parse_codex_sse(text: &str) -> SpeechResult<Value> {
    let mut completed: Option<Value> = None;
    let mut deltas = String::new();
    let mut streamed_output = Vec::new();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event: Value = serde_json::from_str(data).map_err(|error| {
            SpeechError::Request(format!(
                "Codex speech prep stream returned invalid JSON: {error}"
            ))
        })?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    deltas.push_str(delta);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item").cloned() {
                    streamed_output.push(item);
                }
            }
            Some("response.completed") => {
                completed = event.get("response").cloned();
            }
            Some("response.failed") | Some("response.incomplete") => {
                return Err(SpeechError::Request(format!(
                    "Codex speech prep stream ended with {}",
                    event.get("type").and_then(Value::as_str).unwrap_or("error")
                )));
            }
            _ => {}
        }
    }
    let mut payload = completed.ok_or_else(|| {
        SpeechError::Request("Codex speech prep stream ended without completion event".into())
    })?;
    if payload.get("output_text").and_then(Value::as_str).is_none() && !deltas.is_empty() {
        payload["output_text"] = Value::String(deltas);
    }
    if !streamed_output.is_empty() && payload.get("output").and_then(Value::as_array).is_none() {
        payload["output"] = Value::Array(streamed_output);
    }
    Ok(payload)
}

fn extract_codex_text(payload: &Value) -> Option<String> {
    if let Some(text) = payload.get("output_text").and_then(Value::as_str) {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    let output = payload.get("output")?.as_array()?;
    let text = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flat_map(|content| content.iter())
        .filter_map(|block| {
            let block_type = block.get("type").and_then(Value::as_str);
            (block_type == Some("output_text") || block_type == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn normalize_codex_base_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    normalized
        .strip_suffix("/responses")
        .unwrap_or(normalized)
        .to_string()
}

fn chrono_like_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_codex_sse_deltas() {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"[softly] hi\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
        );

        let payload = parse_codex_sse(sse).unwrap();

        assert_eq!(extract_codex_text(&payload).unwrap(), "[softly] hi");
    }

    #[test]
    fn normalizes_codex_base_url() {
        assert_eq!(
            normalize_codex_base_url("https://chatgpt.com/backend-api/codex/responses/"),
            "https://chatgpt.com/backend-api/codex"
        );
    }

    #[test]
    fn codex_response_body_strips_provider_prefix_from_model() {
        let body = codex_responses_body("codex/gpt-5.3-codex-spark", Some("medium"), "tag this");

        assert_eq!(body["model"], "gpt-5.3-codex-spark");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }
}
