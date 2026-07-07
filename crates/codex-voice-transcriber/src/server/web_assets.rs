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
#[cfg(test)]
pub(crate) fn web_dist_is_stub() -> bool {
    env!("CODEX_VOICE_WEB_DIST_KIND").as_bytes() == b"stub"
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
    self.registration
      .unregister()
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
/// contain symlinks, so both the root and the resolved target are canonicalized
/// and the target is required to remain within the root.
async fn read_override_asset(root: &Path, rel: &str) -> Option<Vec<u8>> {
    let canonical_root = tokio::fs::canonicalize(root).await.ok()?;
    let target = canonical_root.join(rel);
    let canonical_target = tokio::fs::canonicalize(&target).await.ok()?;
    if !canonical_target.starts_with(&canonical_root) {
        return None;
    }
    tokio::fs::read(&canonical_target).await.ok()
}

fn asset_response(path: &str, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type_for(path)),
            (header::CACHE_CONTROL, cache_control_for(path).to_string()),
        ],
        body,
    )
        .into_response()
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
