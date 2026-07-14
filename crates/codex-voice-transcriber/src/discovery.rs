use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use codex_voice_core::fs::{set_owner_only_directory_permissions, write_private_file_atomic};

use crate::TranscriberError;

const TOKEN_ENV: &str = "CODEX_VOICE_TRANSCRIBER_TOKEN";
pub(super) const URL_ENV: &str = "CODEX_VOICE_TRANSCRIBER_URL";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriberDiscoveryFile {
    pub url: String,
    pub openai_base_url: String,
    pub token: String,
    pub pid: u32,
    #[serde(default)]
    pub capabilities: ServiceCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceCapabilities {
    pub transcriptions: bool,
    pub speech: bool,
    #[serde(default)]
    pub desktop: bool,
}

impl TranscriberDiscoveryFile {
    pub fn new(root_url: String, token: String, capabilities: ServiceCapabilities) -> Self {
        Self {
            openai_base_url: format!("{}/v1", root_url.trim_end_matches('/')),
            url: root_url,
            token,
            pid: std::process::id(),
            capabilities,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiscoveryCandidate {
    pub(super) base_url: String,
    pub(super) token: String,
}

pub(super) fn resolve_discovery_candidate() -> Option<DiscoveryCandidate> {
    discovery_candidate_from_parts(
        env::var(URL_ENV).ok(),
        env::var(TOKEN_ENV).ok(),
        read_discovery_file(),
        pid_is_running,
    )
}

pub(super) fn discovery_candidate_from_parts(
    env_url: Option<String>,
    env_token: Option<String>,
    discovery: Option<TranscriberDiscoveryFile>,
    pid_alive: impl Fn(u32) -> bool,
) -> Option<DiscoveryCandidate> {
    if let Some(url) = env_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
    {
        let token = env_token.and_then(normalize_token).or_else(|| {
            let file = discovery.as_ref()?;
            if discovery_url_matches(&url, file) {
                normalize_token(file.token.clone())
            } else {
                None
            }
        })?;
        return Some(DiscoveryCandidate {
            base_url: url,
            token,
        });
    }

    let discovery = discovery?;
    if !pid_alive(discovery.pid) {
        return None;
    }
    let token = normalize_token(discovery.token)?;
    Some(DiscoveryCandidate {
        base_url: discovery.url,
        token,
    })
}

pub(super) fn discovery_url_matches(url: &str, discovery: &TranscriberDiscoveryFile) -> bool {
    normalize_loopback(root_url(url)) == normalize_loopback(root_url(&discovery.url))
        || normalize_loopback(root_url(url))
            == normalize_loopback(root_url(&discovery.openai_base_url))
}

fn normalize_loopback(url: String) -> String {
    // Scope replacement to the authority (host:port) only, not paths or query params.
    let lower = url.to_lowercase();
    let authority_end = lower
        .find("://")
        .map(|scheme_end| {
            let rest = &lower[scheme_end + 3..];
            let path_start = rest.find('/').unwrap_or(rest.len());
            scheme_end + 3 + path_start
        })
        .unwrap_or(lower.len());
    let (authority, remainder) = lower.split_at(authority_end);
    authority
        .replace("localhost", "127.0.0.1")
        .replace("[::1]", "127.0.0.1")
        + remainder
}

pub fn discovery_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(env::temp_dir)
        .join("codex-voice")
        .join("transcriber.json")
}

pub(super) fn read_discovery_file() -> Option<TranscriberDiscoveryFile> {
    let text = std::fs::read_to_string(discovery_path()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_discovery_file(discovery: &TranscriberDiscoveryFile) -> Result<(), TranscriberError> {
    let path = discovery_path();
    let parent = path.parent().ok_or_else(|| {
        TranscriberError::Discovery("transcriber discovery path has no parent".to_string())
    })?;
    std::fs::create_dir_all(parent).map_err(|source| TranscriberError::Io {
        context: format!("failed to create {}", parent.display()),
        source,
    })?;
    set_owner_only_directory_permissions(parent).map_err(|source| TranscriberError::Io {
        context: format!("failed to restrict {}", parent.display()),
        source,
    })?;
    let tmp_path = path.with_extension(format!(
        "json.{}.tmp",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let text = serde_json::to_string_pretty(discovery)?;
    write_private_file_atomic(&path, &tmp_path, text.as_bytes()).map_err(|source| {
        TranscriberError::Io {
            context: format!("failed to write discovery file {}", path.display()),
            source,
        }
    })?;
    Ok(())
}

pub(super) fn remove_discovery_file_if_current(discovery: &TranscriberDiscoveryFile) {
    let Some(current) = read_discovery_file() else {
        return;
    };
    if current.pid == discovery.pid && current.token == discovery.token {
        let _ = std::fs::remove_file(discovery_path());
    }
}

pub(super) fn pid_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Non-Linux builds do not currently have a cheap, portable stale-PID check.
        // Keep the discovery file usable and let the health probe reject dead services.
        let _ = pid;
        true
    }
}

pub(super) fn resolve_or_generate_token(env_key: &str) -> String {
    env::var(env_key)
        .ok()
        .and_then(normalize_token)
        .unwrap_or_else(|| hex::encode(rand::random::<[u8; 32]>()))
}

pub(super) fn normalize_token(token: String) -> Option<String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

pub(super) fn service_root_url(addr: SocketAddr) -> String {
    let host = match addr.ip() {
        IpAddr::V4(ip) if ip == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        IpAddr::V4(ip) if ip == Ipv4Addr::LOCALHOST => "localhost".to_string(),
        IpAddr::V4(ip) => ip.to_string(),
    };
    format!("http://{host}:{}", addr.port())
}

pub(super) fn root_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    for suffix in [
        "/v1/audio/transcriptions",
        "/audio/transcriptions",
        "/v1/audio/speech",
        "/audio/speech",
        "/v1",
    ] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.trim_end_matches('/').to_string();
        }
    }
    trimmed.to_string()
}

pub(super) fn health_url(base_url: &str) -> String {
    format!("{}/healthz", root_url(base_url))
}

pub(super) fn transcription_url(base_url: &str) -> String {
    format!("{}/v1/audio/transcriptions", root_url(base_url))
}

#[allow(dead_code)]
pub(super) fn speech_url(base_url: &str) -> String {
    format!("{}/v1/audio/speech", root_url(base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_service_urls() {
        assert_eq!(
            health_url("http://127.0.0.1:3845/v1"),
            "http://127.0.0.1:3845/healthz"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845/v1"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845/v1/audio/transcriptions"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            speech_url("http://127.0.0.1:3845"),
            "http://127.0.0.1:3845/v1/audio/speech"
        );
        assert_eq!(
            speech_url("http://127.0.0.1:3845/v1/audio/speech"),
            "http://127.0.0.1:3845/v1/audio/speech"
        );
        assert_eq!(
            root_url("http://127.0.0.1:3845/v1/audio/transcriptions"),
            "http://127.0.0.1:3845"
        );
    }

    #[test]
    fn stale_discovery_is_ignored_without_env_override() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_candidate_from_parts(None, None, Some(discovery), |_| false).is_none());
    }

    #[test]
    fn env_url_can_reuse_discovery_token_for_matching_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        let candidate = discovery_candidate_from_parts(
            Some("http://127.0.0.1:3845/v1".into()),
            None,
            Some(discovery),
            |_| false,
        )
        .expect("env URL with file token resolves");
        assert_eq!(candidate.base_url, "http://127.0.0.1:3845/v1");
        assert_eq!(candidate.token, "from-file");
    }

    #[test]
    fn env_url_requires_explicit_token_for_different_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_candidate_from_parts(
            Some("http://127.0.0.1:9999/v1".into()),
            None,
            Some(discovery),
            |_| true,
        )
        .is_none());
    }

    #[test]
    fn env_url_uses_explicit_token_for_different_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        let candidate = discovery_candidate_from_parts(
            Some("http://127.0.0.1:9999/v1".into()),
            Some("from-env".into()),
            Some(discovery),
            |_| true,
        )
        .expect("explicit token resolves");
        assert_eq!(candidate.token, "from-env");
    }

    #[test]
    fn discovery_tokens_are_trimmed() {
        assert_eq!(
            normalize_token("  test-token\n".to_string()).as_deref(),
            Some("test-token")
        );
        assert!(normalize_token(" \n\t ".to_string()).is_none());
    }

    #[test]
    fn localhost_matches_discovery_127_0_0_1() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://localhost:3845", &discovery));
    }

    #[test]
    fn ipv6_loopback_matches_discovery_127_0_0_1() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://[::1]:3845", &discovery));
    }

    #[test]
    fn mixed_case_localhost_matches_discovery() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://LOCALHOST:3845", &discovery));
        assert!(discovery_url_matches("http://Localhost:3845", &discovery));
    }
}
