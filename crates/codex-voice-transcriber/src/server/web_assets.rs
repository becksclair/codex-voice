//! Serving layer for the built web UI.
//!
//! The interface is a Vite/React app whose production build lands in
//! `web/dist`. `build.rs` stages that output (or a stub when it has not been
//! built) into `$OUT_DIR/web-dist`, which is embedded here via `include_dir!`.
//! A `--web-dist` override directory can shadow the embedded copy for local
//! development without a rebuild.

use std::path::Path;

use axum::{
    extract::{Path as AxumPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use include_dir::{include_dir, Dir};

use super::ServiceState;

static WEB_DIST: Dir<'static> = include_dir!("$OUT_DIR/web-dist");

const INDEX_PATH: &str = "index.html";

/// Whether the embedded web dist is the build-time placeholder rather than a
/// real build. Content-dependent tests skip themselves when this is true.
pub(crate) fn web_dist_is_stub() -> bool {
    env!("CODEX_VOICE_WEB_DIST_KIND").as_bytes() == b"stub"
}

pub(crate) fn web_ui_available(override_dir: Option<&Path>) -> bool {
    match override_dir {
        Some(path) => override_web_ui_available(path),
        None => embedded_web_ui_available(),
    }
}

pub(crate) async fn web_ui_available_async(override_dir: Option<std::path::PathBuf>) -> bool {
    match override_dir {
        Some(path) => tokio::task::spawn_blocking(move || override_web_ui_available(&path))
            .await
            .unwrap_or(false),
        None => embedded_web_ui_available(),
    }
}

fn embedded_web_ui_available() -> bool {
    if web_dist_is_stub() {
        return false;
    }
    let Some(index) = WEB_DIST
        .get_file(INDEX_PATH)
        .and_then(|file| file.contents_utf8())
    else {
        return false;
    };
    referenced_web_assets_available(index, |relative| WEB_DIST.get_file(relative).is_some())
}

fn override_web_ui_available(root: &Path) -> bool {
    let Ok(index) = std::fs::read_to_string(root.join(INDEX_PATH)) else {
        return false;
    };
    referenced_web_assets_available(&index, |relative| root.join(relative).is_file())
}

fn referenced_web_assets_available(index: &str, mut exists: impl FnMut(&Path) -> bool) -> bool {
    let mut saw_script = false;
    for token in index.split(['"', '\'']) {
        let Some(relative) = token.strip_prefix("/web/") else {
            continue;
        };
        let relative = relative
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim_start_matches('/');
        if relative.is_empty() {
            continue;
        }
        let path = Path::new(relative);
        if path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || !exists(path)
        {
            return false;
        }
        if relative.starts_with("assets/") && relative.ends_with(".js") {
            saw_script = true;
        }
    }
    saw_script
}

/// Self-destructing service worker served at the legacy `/web-sw.js` URL.
///
/// The previous single-file PWA registered a service worker here. Installed
/// clients still request it on update, so this unregisters itself and reloads
/// open windows to let them pick up the new dist-served app. Remove after a
/// couple of releases once installed clients have cycled through.
const LEGACY_SERVICE_WORKER_JS: &str = r#"self.addEventListener('install', () => {
  self.skipWaiting();
});
self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((key) => key.startsWith('codex-voice-web-'))
            .map((key) => caches.delete(key))
        )
      )
      // Cache cleanup is best-effort: activate fires once per worker version,
      // so a rejected delete must never block the unregister + reload below.
      .catch(() => {})
      .then(() => self.registration.unregister())
      .then(() => self.clients.matchAll({ type: 'window' }))
      .then((clients) => {
        clients.forEach((client) => client.navigate(client.url));
      })
  );
});
"#;

pub(crate) async fn serve_web_index(State(state): State<ServiceState>) -> Response {
    match load_asset(&state, INDEX_PATH).await {
        Some(body) => asset_response(INDEX_PATH, body),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "web index unavailable").into_response(),
    }
}

pub(crate) async fn serve_web_asset(
    State(state): State<ServiceState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let Some(rel) = sanitize_asset_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(body) = load_asset(&state, &rel).await {
        return asset_response(&rel, body);
    }

    // SPA fallback: an extensionless path is a client-side route, so serve the
    // app shell. Paths with a file extension are concrete assets and 404 when
    // missing rather than masking a broken reference with HTML.
    let last_segment = rel.rsplit('/').next().unwrap_or(&rel);
    if !last_segment.contains('.') {
        if let Some(body) = load_asset(&state, INDEX_PATH).await {
            return asset_response(INDEX_PATH, body);
        }
    }

    StatusCode::NOT_FOUND.into_response()
}

pub(crate) async fn legacy_service_worker() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        LEGACY_SERVICE_WORKER_JS,
    )
        .into_response()
}

async fn load_asset(state: &ServiceState, rel: &str) -> Option<Vec<u8>> {
    match state.web_dist_override.as_deref() {
        Some(root) => read_override_asset(root, rel).await,
        None => WEB_DIST.get_file(rel).map(|file| file.contents().to_vec()),
    }
}

/// Read an asset from the override directory, refusing to escape the root.
///
/// The relative path is already sanitized, but the override directory may
/// contain symlinks, so the resolved target is canonicalized and required to
/// remain within the root, which was canonicalized once when the server bound.
async fn read_override_asset(root: &Path, rel: &str) -> Option<Vec<u8>> {
    let target = root.join(rel);
    let canonical_target = tokio::fs::canonicalize(&target).await.ok()?;
    if !canonical_target.starts_with(root) {
        return None;
    }
    tokio::fs::read(&canonical_target).await.ok()
}

fn asset_response(path: &str, body: Vec<u8>) -> Response {
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type_for(path)),
            (header::CACHE_CONTROL, cache_control_for(path).to_string()),
        ],
        body,
    )
        .into_response();
    // The app's canonical URL is /web (no trailing slash). A worker script at
    // /web/sw.js may only claim /web/ by default, which does not cover /web
    // itself; this header authorizes the wider registration scope /web used by
    // the frontend so the installed PWA is controlled (and works offline).
    if path == "sw.js" {
        response.headers_mut().insert(
            header::HeaderName::from_static("service-worker-allowed"),
            header::HeaderValue::from_static("/web"),
        );
    }
    response
}

fn content_type_for(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".webmanifest") {
        return "application/manifest+json".to_string();
    }
    if lower.ends_with(".js") || lower.ends_with(".mjs") {
        return "text/javascript; charset=utf-8".to_string();
    }
    if lower.ends_with(".html") {
        return "text/html; charset=utf-8".to_string();
    }
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// Cache policy derived purely from where a file sits in the dist tree. Files
/// under `assets/` are content-hashed by the bundler and safe to cache forever;
/// everything else (index.html, manifests, the service worker, icons at the
/// root) must revalidate so new deploys are picked up. This enforces the repo
/// rule that only content-hashed URLs may be marked immutable.
fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Find the first embedded file whose dist-relative path starts with `prefix`.
/// Used by tests to locate a content-hashed asset without hard-coding a name.
#[cfg(test)]
pub(crate) fn first_embedded_asset_under(prefix: &str) -> Option<String> {
    fn walk(dir: &Dir, prefix: &str) -> Option<String> {
        for file in dir.files() {
            let path = file.path().to_string_lossy().replace('\\', "/");
            if path.starts_with(prefix) {
                return Some(path);
            }
        }
        for sub in dir.dirs() {
            if let Some(found) = walk(sub, prefix) {
                return Some(found);
            }
        }
        None
    }
    walk(&WEB_DIST, prefix)
}

/// Normalize a request path into a safe relative dist path, or reject it.
///
/// Rejects absolute paths, backslashes, and any empty, `.`, or `..` segment so
/// a request can never traverse outside the dist root.
fn sanitize_asset_path(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\\') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        segments.push(segment);
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn referenced_asset_graph_requires_every_file_and_a_script() {
        let index =
            r#"<link href="/web/assets/app.css"><script src="/web/assets/app.js"></script>"#;
        assert!(referenced_web_assets_available(index, |path| {
            matches!(path.to_str(), Some("assets/app.css" | "assets/app.js"))
        }));
        assert!(!referenced_web_assets_available(index, |path| {
            path == Path::new("assets/app.css")
        }));
        assert!(!referenced_web_assets_available(
            r#"<link href="/web/assets/app.css">"#,
            |_| true
        ));
    }
}
