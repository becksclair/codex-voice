use async_trait::async_trait;
use codex_voice_core::{
    RecordedAudio, TranscriptionClient, TranscriptionError, TranscriptionResult,
};
use reqwest::multipart;
use serde::Deserialize;
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const TRANSCRIBE_URL: &str = "https://chatgpt.com/backend-api/transcribe";

#[derive(Debug, Clone)]
pub struct CodexAuth {
    pub access_token: String,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexAuthService {
    auth_path: PathBuf,
}

impl CodexAuthService {
    pub fn new() -> TranscriptionResult<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| TranscriptionError::Auth("failed to locate home directory".into()))?;
        Ok(Self {
            auth_path: home.join(".codex").join("auth.json"),
        })
    }

    pub fn with_auth_path(auth_path: PathBuf) -> Self {
        Self { auth_path }
    }

    pub fn read(&self) -> TranscriptionResult<CodexAuth> {
        let text = fs::read_to_string(&self.auth_path).map_err(|error| {
            TranscriptionError::Auth(format!(
                "failed to read {}: {error}",
                self.auth_path.display()
            ))
        })?;
        let auth: AuthFile = serde_json::from_str(&text).map_err(|error| {
            TranscriptionError::Auth(format!("failed to parse auth.json: {error}"))
        })?;
        let access_token = auth
            .tokens
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                TranscriptionError::Auth("auth.json has no tokens.access_token".into())
            })?;
        Ok(CodexAuth {
            access_token,
            account_id: auth.tokens.account_id,
        })
    }

    pub fn refresh(&self) -> TranscriptionResult<()> {
        let codex = resolve_codex_cli();
        let mut child = Command::new(&codex)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                TranscriptionError::Auth(format!("failed to spawn `{}`: {error}", codex.display()))
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            terminate_child(&mut child);
            TranscriptionError::Auth("failed to open codex stdout".into())
        })?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            terminate_child(&mut child);
            TranscriptionError::Auth("failed to open codex stdin".into())
        })?;
        if let Err(error) = stdin.write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-voice","version":"0.1.0"}}}
{"jsonrpc":"2.0","id":2,"method":"account/read","params":{"refreshToken":true}}
"#,
        ) {
            terminate_child(&mut child);
            return Err(TranscriptionError::Auth(format!(
                "failed to write to codex: {error}"
            )));
        }
        drop(stdin);

        wait_for_account_read(&mut child, stdout, Duration::from_secs(10))
    }

    pub fn read_or_refresh(&self) -> TranscriptionResult<CodexAuth> {
        match self.read() {
            Ok(auth) => Ok(auth),
            Err(first_error) => {
                self.refresh()?;
                self.read().map_err(|second_error| {
                    TranscriptionError::Auth(format!(
                        "initial read failed ({first_error}); refresh completed but reread failed ({second_error})"
                    ))
                })
            }
        }
    }

    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }
}

fn wait_for_account_read(
    child: &mut Child,
    stdout: impl std::io::Read + Send + 'static,
    timeout: Duration,
) -> TranscriptionResult<()> {
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                return Err(TranscriptionError::Auth(format!(
                    "codex auth refresh exited before account/read completed; status={status}"
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(TranscriptionError::Auth(format!(
                    "failed while polling codex auth refresh: {error}"
                )));
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            terminate_child(child);
            return Err(TranscriptionError::Auth(format!(
                "timed out after {}s waiting for codex account/read response",
                timeout.as_secs()
            )));
        }

        match rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(Ok(line)) if is_account_read_response(&line) => {
                terminate_child(child);
                return Ok(());
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                terminate_child(child);
                return Err(TranscriptionError::Auth(format!(
                    "failed reading codex auth refresh response: {error}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate_child(child);
                return Err(TranscriptionError::Auth(
                    "codex auth refresh stdout closed before account/read completed".into(),
                ));
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn is_account_read_response(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    value.get("id").and_then(|id| id.as_i64()) == Some(2)
        && value.get("result").is_some()
        && value.get("error").is_none()
}

#[derive(Debug, Clone)]
pub struct CodexTranscriptionClient {
    auth: CodexAuthService,
    http: reqwest::Client,
}

impl CodexTranscriptionClient {
    pub fn new(auth: CodexAuthService) -> TranscriptionResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| {
                TranscriptionError::Request(format!("failed to build HTTP client: {error}"))
            })?;
        Ok(Self { auth, http })
    }
}

#[async_trait]
impl TranscriptionClient for CodexTranscriptionClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        let auth = self.auth.read_or_refresh()?;
        let bytes = fs::read(&recording.path).map_err(|error| {
            TranscriptionError::Request(format!(
                "failed to read {}: {error}",
                recording.path.display()
            ))
        })?;
        let file_part = multipart::Part::bytes(bytes)
            .file_name(recording.filename.clone())
            .mime_str(&recording.content_type)
            .map_err(|error| {
                TranscriptionError::Request(format!("invalid multipart mime: {error}"))
            })?;
        let form = multipart::Form::new().part("file", file_part);

        let mut request = self
            .http
            .post(TRANSCRIBE_URL)
            .bearer_auth(&auth.access_token)
            .header("originator", "Codex Desktop")
            .header("User-Agent", "Codex Voice/0.1.0")
            .header("Accept", "application/json")
            .multipart(form);
        if let Some(account_id) = auth.account_id.as_deref() {
            request = request.header("ChatGPT-Account-Id", account_id);
        }

        let response = request
            .send()
            .await
            .map_err(|error| TranscriptionError::Request(error.to_string()))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            TranscriptionError::Request(format!("failed to read response: {error}"))
        })?;
        if !status.is_success() {
            return Err(TranscriptionError::Request(format!(
                "transcription failed with HTTP {status}: {}",
                redact(&text)
            )));
        }
        parse_transcript(&text)
    }
}

pub fn parse_transcript(body: &str) -> TranscriptionResult<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["text", "transcript"] {
            if let Some(text) = value.get(key).and_then(|value| value.as_str()) {
                return Ok(text.to_string());
            }
        }
        return Err(TranscriptionError::Request(
            "transcription response JSON did not include text".into(),
        ));
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        Err(TranscriptionError::Request(
            "empty transcription response".into(),
        ))
    } else {
        Ok(trimmed.to_string())
    }
}

fn resolve_codex_cli() -> PathBuf {
    if let Ok(path) = env::var("CODEX_CLI_PATH") {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "macos")]
    {
        let app_path = PathBuf::from("/Applications/Codex.app/Contents/Resources/codex");
        if app_path.exists() {
            return app_path;
        }
    }
    PathBuf::from("codex")
}

fn redact(text: &str) -> String {
    const MAX: usize = 300;
    let clipped: String = text.chars().take(MAX).collect();
    clipped.replace("access_token", "access_token(redacted)")
}

#[derive(Debug, Deserialize)]
struct AuthFile {
    tokens: AuthTokens,
}

#[derive(Debug, Deserialize)]
struct AuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_account_read_response_with_whitespace() {
        assert!(is_account_read_response(
            r#"{"jsonrpc":"2.0","id": 2,"result":{"ok":true}}"#
        ));
    }

    #[test]
    fn ignores_other_jsonrpc_responses() {
        assert!(!is_account_read_response(
            r#"{"jsonrpc":"2.0","id": 1,"result":{"ok":true}}"#
        ));
    }

    #[test]
    fn rejects_account_read_error_response() {
        assert!(!is_account_read_response(
            r#"{"jsonrpc":"2.0","id": 2,"error":{"message":"nope"}}"#
        ));
    }

    #[test]
    fn rejects_account_read_response_without_result() {
        assert!(!is_account_read_response(r#"{"jsonrpc":"2.0","id": 2}"#));
    }

    #[test]
    fn parses_json_transcript_text() {
        assert_eq!(
            parse_transcript(r#"{"text":"hello"}"#).unwrap(),
            "hello".to_string()
        );
    }

    #[test]
    fn rejects_json_without_transcript_text() {
        assert!(parse_transcript(r#"{"status":"ok"}"#).is_err());
    }
}
