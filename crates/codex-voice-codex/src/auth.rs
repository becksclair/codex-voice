use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use serde::Deserialize;

use codex_voice_core::{TranscriptionError, TranscriptionResult};

const AUTH_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct CodexAuth {
    pub access_token: String,
    pub account_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexAuthService {
    auth_path: PathBuf,
    cache: Arc<RwLock<Option<(CodexAuth, Instant)>>>,
}

impl CodexAuthService {
    pub fn new() -> TranscriptionResult<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| TranscriptionError::Auth("failed to locate home directory".into()))?;
        Ok(Self {
            auth_path: home.join(".codex").join("auth.json"),
            cache: Arc::new(RwLock::new(None)),
        })
    }

    pub fn with_auth_path(auth_path: PathBuf) -> Self {
        Self {
            auth_path,
            cache: Arc::new(RwLock::new(None)),
        }
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
        // Fast path: return cached auth if still fresh.
        if let Ok(guard) = self.cache.read() {
            if let Some((auth, fetched_at)) = guard.as_ref() {
                if fetched_at.elapsed() < AUTH_CACHE_TTL {
                    return Ok(auth.clone());
                }
            }
        }

        match self.read() {
            Ok(auth) => {
                if let Ok(mut guard) = self.cache.write() {
                    *guard = Some((auth.clone(), Instant::now()));
                }
                Ok(auth)
            }
            Err(first_error) => {
                self.refresh()?;
                let auth = self.read().map_err(|second_error| {
                    TranscriptionError::Auth(format!(
                        "initial read failed ({first_error}); refresh completed but reread failed ({second_error})"
                    ))
                })?;
                if let Ok(mut guard) = self.cache.write() {
                    *guard = Some((auth.clone(), Instant::now()));
                }
                Ok(auth)
            }
        }
    }

    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }
}

fn wait_for_account_read(
    child: &mut Child,
    stdout: impl std::io::Read,
    timeout: Duration,
) -> TranscriptionResult<()> {
    let deadline = Instant::now() + timeout;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

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

        // Read one line with a timeout by setting a read timeout on the underlying handle.
        // Since BufReader doesn't support timeouts directly, we use a short non-blocking poll
        // pattern: try to read a line, and if we get WouldBlock, sleep briefly and retry.
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF — stdout closed
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Ok(_) => {
                if is_account_read_response(&line) {
                    terminate_child(child);
                    return Ok(());
                }
                line.clear();
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                terminate_child(child);
                return Err(TranscriptionError::Auth(format!(
                    "failed reading codex auth refresh response: {error}"
                )));
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
}
