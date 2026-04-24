use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

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

impl PortalTokenStore {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            path: portal_tokens_path()?,
        })
    }

    #[cfg(test)]
    pub fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> io::Result<Option<PersistedPortalToken>> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => {
                let parsed =
                    serde_json::from_str::<PersistedPortalToken>(&raw).map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "failed to parse persisted portal token file {}: {error}",
                                self.path.display()
                            ),
                        )
                    })?;
                Ok(Some(parsed))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, record: &PersistedPortalToken) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            set_owner_only_directory_permissions(parent)?;
        }

        let payload = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        let tmp_path = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let mut tmp_file = options.open(&tmp_path)?;
        tmp_file.write_all(&payload)?;
        tmp_file.sync_all()?;
        drop(tmp_file);
        set_owner_only_file_permissions(&tmp_path)?;
        fs::rename(&tmp_path, &self.path)?;
        set_owner_only_file_permissions(&self.path)?;
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

fn set_owner_only_directory_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }

    Ok(())
}

fn set_owner_only_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
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
