use codex_voice_core::{fs::set_owner_only_directory_permissions, PlatformError, PlatformResult};
use serde::{Deserialize, Serialize};
use std::{
    fmt::Display,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static TOKEN_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPortalToken {
    pub restore_token: String,
    pub updated_at_unix_secs: u64,
    pub xdg_session_type: Option<String>,
    pub compositor: Option<String>,
    pub remote_desktop_version: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct PortalTokenStore {
    path: PathBuf,
}

/// Wrap an error with context into a `PlatformError::Message`.
fn ctx(context: impl Into<String>, source: impl Display) -> PlatformError {
    PlatformError::Message(format!("{}: {source}", context.into()))
}

impl PortalTokenStore {
    pub fn new() -> PlatformResult<Self> {
        Ok(Self {
            path: portal_tokens_path()
                .map_err(|e| ctx("failed to resolve portal token path", e))?,
        })
    }

    #[cfg(test)]
    pub fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> PlatformResult<Option<PersistedPortalToken>> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => {
                let parsed = serde_json::from_str::<PersistedPortalToken>(&raw).map_err(|e| {
                    ctx(
                        format!(
                            "failed to parse persisted portal token file {}",
                            self.path.display()
                        ),
                        e,
                    )
                })?;
                Ok(Some(parsed))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ctx(
                format!("failed to read portal token file {}", self.path.display()),
                error,
            )),
        }
    }

    pub fn save(&self, record: &PersistedPortalToken) -> PlatformResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| ctx("failed to create portal token dir", e))?;
            set_owner_only_directory_permissions(parent)
                .map_err(|e| ctx("failed to restrict portal token dir", e))?;
        }

        let payload = serde_json::to_vec_pretty(record)
            .map_err(|e| ctx("failed to serialize portal token", e))?;
        let tmp_path = self.path.with_extension(format!("{}.tmp", temp_suffix()));
        codex_voice_core::fs::write_private_file_atomic(&self.path, &tmp_path, &payload)
            .map_err(|e| ctx("failed to write portal token file", e))?;
        Ok(())
    }
}

pub fn new_token_record(
    restore_token: String,
    remote_desktop_version: Option<u32>,
) -> PersistedPortalToken {
    PersistedPortalToken {
        restore_token,
        updated_at_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        xdg_session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        compositor: compositor_hint(),
        remote_desktop_version,
    }
}

fn portal_tokens_path() -> io::Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HOME is unset and XDG_STATE_HOME is not available",
                )
            })?;
            PathBuf::from(home).join(".local/state")
        }
    };
    Ok(base.join("codex-voice").join("portal-tokens.json"))
}

fn temp_suffix() -> String {
    let counter = TOKEN_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{counter}", std::process::id())
}

fn compositor_hint() -> Option<String> {
    let xdg_current_desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let desktop_session = std::env::var("DESKTOP_SESSION").ok();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    match (xdg_current_desktop, desktop_session, wayland_display) {
        (Some(desktop), _, Some(_))
            if desktop.to_ascii_lowercase().contains("kde")
                || desktop.to_ascii_lowercase().contains("plasma") =>
        {
            Some("kde-kwin-wayland".to_string())
        }
        (Some(desktop), _, _) => Some(desktop),
        (None, Some(session), _) => Some(session),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{PersistedPortalToken, PortalTokenStore};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "codex-voice-portal-token-test-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ))
            .join("portal-tokens.json")
    }

    #[test]
    fn saves_and_loads_token_records() {
        let path = temp_path("roundtrip");
        let store = PortalTokenStore::for_path(path.clone());
        let record = PersistedPortalToken {
            restore_token: "token-1".to_string(),
            updated_at_unix_secs: 123,
            xdg_session_type: Some("wayland".to_string()),
            compositor: Some("kde-kwin-wayland".to_string()),
            remote_desktop_version: Some(2),
        };

        store.save(&record).expect("record should save");
        let loaded = store.load().expect("record should load");
        assert_eq!(loaded, Some(record));
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}
