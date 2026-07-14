//! Tauri-backed [`AppWindows`] implementation: opens/focuses the main and
//! settings webview windows, both of which load the standalone web frontend
//! (`web/`) over HTTP rather than bundled assets.

use std::sync::Arc;

use codex_voice_transcriber::client::LocalTranscriberClient;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::{tray::AppWindows, DESKTOP_ORIGIN};

const MAIN_LABEL: &str = "main";
const SETTINGS_LABEL: &str = "settings";

/// Opens/focuses the app's webview windows on the Tauri main thread. All
/// Window creation, focus, and navigation calls are dispatched via
/// `AppHandle::run_on_main_thread` because window operations off the main
/// thread crash GTK on Linux.
pub struct DesktopWindows {
    app: tauri::AppHandle,
    base_url: Arc<str>,
    client: LocalTranscriberClient,
}

impl DesktopWindows {
    pub fn new(app: tauri::AppHandle, client: LocalTranscriberClient) -> Self {
        let base_url = DESKTOP_ORIGIN.into();
        Self {
            app,
            base_url,
            client,
        }
    }

    /// Focuses the window with `label` if it exists, otherwise creates it
    /// pointed at `url`. When `renavigate_on_focus` is set, an existing window
    /// is navigated to `url` instead of merely focused — used for the speak
    /// intake so the one-shot `#intent` identifier is delivered by a real load
    /// of the exact URL, even when a prior navigation is still pending. The
    /// whole operation is dispatched to the Tauri main thread.
    fn open_or_focus(
        &self,
        label: &'static str,
        title: &'static str,
        size: (f64, f64),
        url: String,
        renavigate_on_focus: bool,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        let handle = self.app.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.app
            .run_on_main_thread(move || {
                let result = (|| -> Result<(), String> {
                    let parsed = url
                        .parse()
                        .map_err(|error| format!("failed to parse {label} window URL: {error}"))?;
                    if let Some(window) = handle.get_webview_window(label) {
                        if let Err(error) = window.set_focus() {
                            tracing::warn!(%error, label, "failed to focus window");
                        }
                        if renavigate_on_focus {
                            window.navigate(parsed).map_err(|error| {
                                format!("failed to navigate {label} window: {error}")
                            })?;
                        }
                        return Ok(());
                    }
                    let builder =
                        WebviewWindowBuilder::new(&handle, label, WebviewUrl::External(parsed))
                            .title(title)
                            .inner_size(size.0, size.1);
                    builder
                        .build()
                        .map_err(|error| format!("failed to build {label} window: {error}"))?;
                    Ok(())
                })();
                if let Err(error) = &result {
                    tracing::warn!(%error, label, "window operation failed");
                }
                let _ = result_tx.send(result);
            })
            .map_err(|error| format!("failed to dispatch {label} window operation: {error}"))?;
        Ok(result_rx)
    }
}

const MAIN_TITLE: &str = "Codex Voice";
const MAIN_SIZE: (f64, f64) = (960.0, 720.0);
const SETTINGS_TITLE: &str = "Codex Voice Settings";
const SETTINGS_SIZE: (f64, f64) = (480.0, 360.0);

#[async_trait::async_trait]
impl AppWindows for DesktopWindows {
    fn open_main(&self) {
        let url = format!("{}/web?app=1", self.base_url);
        if let Err(error) = self.open_or_focus(MAIN_LABEL, MAIN_TITLE, MAIN_SIZE, url, false) {
            tracing::warn!(%error, "failed to open main window");
        }
    }

    async fn open_main_with_speak(&self, text: String) -> Result<(), String> {
        let intent_id = self.client.create_desktop_intent(&text).await?;
        let url = format!("{}/web?app=1#intent={intent_id}", self.base_url);
        let (result, delete_on_error) = match self
            .open_or_focus(MAIN_LABEL, MAIN_TITLE, MAIN_SIZE, url, true)
        {
            Ok(completion) => {
                match tokio::time::timeout(std::time::Duration::from_secs(5), completion).await {
                    Ok(Ok(result)) => (result, true),
                    Ok(Err(_)) => (Err("main-window operation was cancelled".to_string()), true),
                    Err(_) => (Err("main-window operation timed out".to_string()), false),
                }
            }
            Err(error) => (Err(error), true),
        };
        if result.is_err() && delete_on_error {
            self.client.delete_desktop_intent(&intent_id).await;
        }
        result
    }

    fn open_settings(&self) {
        let url = format!("{}/web?app=1&view=settings", self.base_url);
        if let Err(error) =
            self.open_or_focus(SETTINGS_LABEL, SETTINGS_TITLE, SETTINGS_SIZE, url, false)
        {
            tracing::warn!(%error, "failed to open settings window");
        }
    }
}

/// Tray fallback when no speech service could be resolved: window requests
/// are logged and dropped instead of opening a webview onto a dead URL.
pub struct UnavailableWindows;

#[async_trait::async_trait]
impl AppWindows for UnavailableWindows {
    fn open_main(&self) {
        tracing::warn!("speech service unavailable; cannot open the main window");
    }

    async fn open_main_with_speak(&self, _text: String) -> Result<(), String> {
        Err("speech service unavailable; cannot open the speak window".into())
    }

    fn open_settings(&self) {
        tracing::warn!("speech service unavailable; cannot open the settings window");
    }
}
