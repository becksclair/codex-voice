use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use codex_voice_core::{SpeechError, SpeechResult};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

pub const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_REFRESH_SKEW_SECONDS: u64 = 300;
static CODEX_AUTH_FILE_WRITE_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub struct CodexLlmClient {
    client: Client,
    auth_file: PathBuf,
    base_url: String,
    /// Serializes token refreshes so concurrent callers that both observe an
    /// expired access token do not each hit the refresh endpoint.
    refresh_guard: Arc<Mutex<()>>,
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
            refresh_guard: Arc::new(Mutex::new(())),
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
        let payload = self.read_auth_payload().await?;
        let tokens = extract_tokens(&payload)?;
        if !force_refresh && !access_token_needs_refresh(&tokens.access_token) {
            return Ok(tokens);
        }

        // Serialize refreshes: only one caller performs the refresh + write at a
        // time. The fast path above stays unlocked.
        let _guard = self.refresh_guard.lock().await;

        // Double-checked locking: a concurrent refresh may have already produced
        // fresh tokens while we waited for the guard. Re-read and re-check before
        // spending a second network round-trip.
        let payload = self.read_auth_payload().await?;
        let tokens = extract_tokens(&payload)?;
        if !force_refresh && !access_token_needs_refresh(&tokens.access_token) {
            return Ok(tokens);
        }

        let refreshed = refresh_tokens(&self.client, &payload, &tokens.refresh_token).await?;
        let result_tokens = extract_tokens(&refreshed)?;
        let auth_file = self.auth_file.clone();
        let written = tokio::task::spawn_blocking(move || {
            write_auth_payload_if_not_older(&auth_file, &refreshed)
        })
        .await
        .map_err(|error| SpeechError::Auth(format!("auth write task failed: {error}")))??;
        if written {
            Ok(result_tokens)
        } else {
            extract_tokens(&self.read_auth_payload().await?)
        }
    }

    async fn read_auth_payload(&self) -> SpeechResult<Value> {
        let auth_file = self.auth_file.clone();
        tokio::task::spawn_blocking(move || read_auth_file(&auth_file))
            .await
            .map_err(|error| SpeechError::Auth(format!("auth read task failed: {error}")))?
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthSnapshot {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthSyncResult {
    Updated,
    Unchanged,
    RejectedOlder,
    RejectedAccount,
    RejectedInvalid,
}

type CodexAuthTokens = CodexAuthSnapshot;

/// Read the complete refresh-capable Codex OAuth bundle without invoking the
/// Codex CLI. Browser-facing config uses this only as a bootstrap snapshot;
/// callers must never log the returned values.
pub fn read_codex_auth_snapshot(path: &Path) -> SpeechResult<CodexAuthSnapshot> {
    let payload = read_auth_file(path)?;
    extract_tokens(&payload)
}

/// Atomically merge a browser-refreshed Codex bundle into the canonical auth
/// file. Only the same account may be updated, and an older access token can
/// never replace a newer one.
pub fn sync_codex_auth_snapshot(
    path: &Path,
    incoming: &CodexAuthSnapshot,
) -> SpeechResult<CodexAuthSyncResult> {
    let _guard = CODEX_AUTH_FILE_WRITE_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut payload = read_auth_file(path)?;
    let current = extract_tokens(&payload)?;
    if current.account_id != incoming.account_id {
        return Ok(CodexAuthSyncResult::RejectedAccount);
    }
    let Some(incoming_expiry) = access_token_expiry(&incoming.access_token) else {
        return Ok(CodexAuthSyncResult::RejectedInvalid);
    };
    if access_token_expiry(&current.access_token).is_some_and(|expiry| incoming_expiry < expiry) {
        return Ok(CodexAuthSyncResult::RejectedOlder);
    }
    if current == *incoming {
        return Ok(CodexAuthSyncResult::Unchanged);
    }

    let token_object = payload
        .as_object_mut()
        .and_then(|object| object.get_mut("tokens"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| SpeechError::Auth("Codex auth file has no token object".into()))?;
    token_object.insert(
        "access_token".into(),
        Value::String(incoming.access_token.clone()),
    );
    token_object.insert(
        "refresh_token".into(),
        Value::String(incoming.refresh_token.clone()),
    );
    token_object.insert(
        "account_id".into(),
        Value::String(incoming.account_id.clone()),
    );
    payload
        .as_object_mut()
        .expect("auth payload checked above")
        .insert(
            "last_refresh".into(),
            Value::String(format!("{}", chrono_like_timestamp())),
        );
    write_auth_file_unlocked(path, &payload)?;
    Ok(CodexAuthSyncResult::Updated)
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

fn write_auth_payload_if_not_older(path: &Path, payload: &Value) -> SpeechResult<bool> {
    let _guard = CODEX_AUTH_FILE_WRITE_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let incoming = extract_tokens(payload)?;
    if let Ok(current_payload) = read_auth_file(path) {
        let current = extract_tokens(&current_payload)?;
        if current.account_id != incoming.account_id {
            return Err(SpeechError::Auth(
                "refreshed Codex auth account no longer matches the configured account".into(),
            ));
        }
        if matches!(
            (access_token_expiry(&incoming.access_token), access_token_expiry(&current.access_token)),
            (Some(incoming), Some(current)) if incoming < current
        ) {
            return Ok(false);
        }
    }
    write_auth_file_unlocked(path, payload)?;
    Ok(true)
}

#[cfg(test)]
fn write_auth_file(path: &Path, payload: &Value) -> SpeechResult<()> {
    let _guard = CODEX_AUTH_FILE_WRITE_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_auth_file_unlocked(path, payload)
}

fn write_auth_file_unlocked(path: &Path, payload: &Value) -> SpeechResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            SpeechError::Auth(format!("failed to create Codex auth directory: {error}"))
        })?;
    }
    let tmp_path = path.with_file_name(format!(
        ".{}.{}.{:08x}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("auth.json"),
        std::process::id(),
        rand::random::<u32>()
    ));
    let text = serde_json::to_string(payload)
        .map_err(|error| SpeechError::Auth(format!("failed to encode Codex auth file: {error}")))?;
    codex_voice_core::fs::write_private_file_atomic(
        path,
        &tmp_path,
        format!("{text}\n").as_bytes(),
    )
    .map_err(|error| {
        SpeechError::Auth(format!(
            "failed to write refreshed Codex auth file: {error}"
        ))
    })?;
    Ok(())
}

fn access_token_needs_refresh(access_token: &str) -> bool {
    let Some(exp) = access_token_expiry(access_token) else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    exp <= now + CODEX_REFRESH_SKEW_SECONDS
}

fn access_token_expiry(access_token: &str) -> Option<u64> {
    let payload = access_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get("exp")
        .and_then(Value::as_u64)
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

    fn access_token_with_expiry(exp: u64) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({ "exp": exp })).unwrap());
        format!("header.{payload}.signature")
    }

    fn unexpired_access_token() -> String {
        access_token_with_expiry(chrono_like_timestamp() + 3600)
    }

    #[test]
    fn browser_auth_sync_updates_same_account_and_preserves_other_fields() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        let current = CodexAuthSnapshot {
            access_token: access_token_with_expiry(100),
            refresh_token: "old-refresh".into(),
            account_id: "acct-123".into(),
        };
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "tokens": {
                    "access_token": current.access_token,
                    "refresh_token": current.refresh_token,
                    "account_id": current.account_id,
                    "id_token": "preserved-id-token"
                },
                "provider": "chatgpt"
            }))
            .unwrap(),
        )
        .expect("write auth fixture");
        let incoming = CodexAuthSnapshot {
            access_token: access_token_with_expiry(200),
            refresh_token: "rotated-refresh".into(),
            account_id: "acct-123".into(),
        };

        assert_eq!(
            sync_codex_auth_snapshot(&path, &incoming).expect("sync succeeds"),
            CodexAuthSyncResult::Updated
        );
        let payload = read_auth_file(&path).expect("read updated auth");
        assert_eq!(extract_tokens(&payload).unwrap(), incoming);
        assert_eq!(payload["tokens"]["id_token"], "preserved-id-token");
        assert_eq!(payload["provider"], "chatgpt");

        let same_access_rotated_refresh = CodexAuthSnapshot {
            refresh_token: "second-rotation".into(),
            ..incoming
        };
        assert_eq!(
            sync_codex_auth_snapshot(&path, &same_access_rotated_refresh).unwrap(),
            CodexAuthSyncResult::Updated
        );
        assert_eq!(
            read_codex_auth_snapshot(&path).unwrap().refresh_token,
            "second-rotation"
        );
    }

    #[test]
    fn browser_auth_sync_rejects_older_or_different_account() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        let current = CodexAuthSnapshot {
            access_token: access_token_with_expiry(200),
            refresh_token: "current-refresh".into(),
            account_id: "acct-123".into(),
        };
        std::fs::write(
            &path,
            serde_json::to_string(&json!({ "tokens": {
                "access_token": current.access_token,
                "refresh_token": current.refresh_token,
                "account_id": current.account_id
            }}))
            .unwrap(),
        )
        .expect("write auth fixture");
        let older = CodexAuthSnapshot {
            access_token: access_token_with_expiry(100),
            refresh_token: "older-refresh".into(),
            account_id: "acct-123".into(),
        };
        assert_eq!(
            sync_codex_auth_snapshot(&path, &older).expect("older bundle is classified"),
            CodexAuthSyncResult::RejectedOlder
        );
        assert_eq!(read_codex_auth_snapshot(&path).unwrap(), current);

        let different_account = CodexAuthSnapshot {
            account_id: "acct-456".into(),
            ..older
        };
        assert_eq!(
            sync_codex_auth_snapshot(&path, &different_account).unwrap(),
            CodexAuthSyncResult::RejectedAccount
        );
        assert_eq!(read_codex_auth_snapshot(&path).unwrap(), current);
    }

    #[test]
    fn server_refresh_write_cannot_replace_a_newer_browser_sync() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        let newer = json!({ "tokens": {
            "access_token": access_token_with_expiry(200),
            "refresh_token": "browser-refresh",
            "account_id": "acct-123"
        }});
        std::fs::write(&path, serde_json::to_string(&newer).unwrap()).expect("write auth fixture");
        let stale_server_refresh = json!({ "tokens": {
            "access_token": access_token_with_expiry(100),
            "refresh_token": "server-refresh",
            "account_id": "acct-123"
        }});

        assert!(
            !write_auth_payload_if_not_older(&path, &stale_server_refresh)
                .expect("stale write is classified")
        );
        assert_eq!(
            read_codex_auth_snapshot(&path).unwrap().refresh_token,
            "browser-refresh"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_tokens_calls_reuse_fresh_token_without_refresh() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        let auth = json!({
            "tokens": {
                "access_token": unexpired_access_token(),
                "refresh_token": "refresh-abc",
                "account_id": "acct-123",
            }
        });
        std::fs::write(&path, serde_json::to_string(&auth).unwrap()).expect("write auth fixture");

        // Point the refresh endpoint at an address that cannot serve a refresh;
        // the fixture token is not expired, so the fast path must return it and
        // neither concurrent caller may attempt a network refresh.
        let client = CodexLlmClient::new(
            path.clone(),
            "http://127.0.0.1:0".to_string(),
            Duration::from_secs(5),
        )
        .expect("client");

        let a = client.clone();
        let b = client.clone();
        let (result_a, result_b) = tokio::join!(a.tokens(false), b.tokens(false));

        let tokens_a = result_a.expect("first tokens() succeeded without refresh");
        let tokens_b = result_b.expect("second tokens() succeeded without refresh");
        assert_eq!(tokens_a.account_id, "acct-123");
        assert_eq!(tokens_b.account_id, "acct-123");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_auth_writes_do_not_corrupt_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");

        let mut handles = Vec::new();
        let mut expected_payloads = Vec::new();
        for i in 0..8 {
            let path = path.clone();
            let payload = json!({ "tokens": { "account_id": format!("task-{i}") } });
            expected_payloads.push(payload.clone());
            handles.push(tokio::task::spawn_blocking(move || {
                write_auth_file(&path, &payload)
            }));
        }
        for handle in handles {
            handle.await.expect("task panicked").expect("write failed");
        }

        let text = std::fs::read_to_string(&path).expect("read final auth file");
        let parsed: Value =
            serde_json::from_str(&text).expect("final auth file must be valid JSON, not torn");
        assert!(
            expected_payloads.contains(&parsed),
            "final payload {parsed:?} did not match any of the 8 writers' payloads"
        );
    }

    #[cfg(unix)]
    #[test]
    fn written_auth_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("auth.json");
        let payload = json!({ "tokens": { "account_id": "task-0" } });

        write_auth_file(&path, &payload).expect("write failed");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
