//! Tray command contract, window-opening abstraction, and the Tauri-backed
//! [`TrayHandle`](crate::app::TrayHandle) implementation.
//!
//! `UiCommand` enumerates the actions a tray menu can request; `AppWindows`
//! abstracts the window layer (implemented by [`crate::windows::DesktopWindows`])
//! so tray/app code can be written and tested against a trait. [`TauriTray`]
//! wires both together into a real system tray icon and menu. `main.rs`'s
//! `run()` calls `TauriTray::start` and `DesktopWindows::new`.

use std::sync::{mpsc, Arc};

use codex_voice_core::DictationState;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};

use crate::app::TrayHandle;
use crate::hud;
use crate::status::{icon_rgba_for_state, UiStatus, ICON_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    StartTestRecording,
    OpenLogs,
    RunDiagnostics,
    Quit,
}

/// Opens the application's windows. Implemented by [`crate::windows::DesktopWindows`].
#[async_trait::async_trait]
pub trait AppWindows: Send + Sync {
    fn open_main(&self);
    async fn open_main_with_speak(&self, text: String) -> Result<(), String>;
    fn open_settings(&self);
}

const MENU_STATUS: &str = "status";
const MENU_TEST_RECORDING: &str = "test-recording";
const MENU_SPEAK_TEXT: &str = "speak-text";
const MENU_SETTINGS: &str = "settings";
const MENU_LOGS: &str = "logs";
const MENU_DIAGNOSTICS: &str = "diagnostics";
const MENU_QUIT: &str = "quit";

/// The effect of a tray menu click, factored out of the `on_menu_event`
/// closure so the id-to-action mapping is unit-testable without a live tray.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MenuAction {
    OpenMain,
    OpenSettings,
    Command(UiCommand),
}

/// Maps a menu item id to the action it triggers. Returns `None` for unknown
/// ids (defensive; every id we register is handled).
fn map_menu_event(id: &str) -> Option<MenuAction> {
    match id {
        MENU_SPEAK_TEXT => Some(MenuAction::OpenMain),
        MENU_SETTINGS => Some(MenuAction::OpenSettings),
        MENU_TEST_RECORDING => Some(MenuAction::Command(UiCommand::StartTestRecording)),
        MENU_LOGS => Some(MenuAction::Command(UiCommand::OpenLogs)),
        MENU_DIAGNOSTICS => Some(MenuAction::Command(UiCommand::RunDiagnostics)),
        MENU_QUIT => Some(MenuAction::Command(UiCommand::Quit)),
        _ => None,
    }
}

/// Tauri-backed system tray: owns the tray icon, its menu, a command channel
/// fed by menu clicks, and a background thread that applies [`UiStatus`]
/// updates (icon/tooltip/menu text + HUD notification) pushed via
/// [`TrayHandle::status_sender`].
pub struct TauriTray {
    app: tauri::AppHandle,
    tray: TrayIcon,
    status_item: MenuItem<tauri::Wry>,
    cmd_rx: mpsc::Receiver<UiCommand>,
    status_tx: mpsc::Sender<UiStatus>,
}

impl TauriTray {
    /// Builds the tray menu/icon on the Tauri main thread (required — tray
    /// and window construction crash GTK off-thread) and starts a background
    /// thread that pumps status updates sent via [`TrayHandle::status_sender`].
    ///
    /// Must be called from a worker thread, never the main thread: it queues
    /// the build closure onto the event loop and blocks on `build_rx.recv()`
    /// until it runs. On the main thread that closure could never run (the
    /// thread is parked in `recv`), so the call would deadlock. The Tauri
    /// event loop must already be running.
    pub fn start(
        initial: UiStatus,
        app: tauri::AppHandle,
        windows: Arc<dyn AppWindows>,
    ) -> anyhow::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<UiCommand>();
        let (build_tx, build_rx) =
            mpsc::channel::<anyhow::Result<(TrayIcon, MenuItem<tauri::Wry>)>>();

        let build_app = app.clone();
        let initial_label = initial.tray_label();
        let initial_state = initial.state.clone();
        app.run_on_main_thread(move || {
            let result = build_tray(&build_app, &initial_label, &initial_state, cmd_tx, windows);
            let _ = build_tx.send(result);
        })
        .map_err(|error| {
            anyhow::anyhow!("failed to schedule tray build on main thread: {error}")
        })?;

        let (tray, status_item) = build_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("tray build task dropped before producing a result"))??;

        // Background tasks push statuses through `status_sender()`; this pump
        // applies them. `update()` (called from the run loop) applies directly
        // instead. Both paths are safe to run concurrently: tray mutations are
        // serialized by the Tauri event loop and the HUD serializes itself on
        // its own worker thread.
        let (status_tx, status_rx) = mpsc::channel::<UiStatus>();
        {
            let app = app.clone();
            let tray = tray.clone();
            let status_item = status_item.clone();
            std::thread::spawn(move || {
                while let Ok(status) = status_rx.recv() {
                    apply_status(&app, &tray, &status_item, &status);
                }
            });
        }

        Ok(Self {
            app,
            tray,
            status_item,
            cmd_rx,
            status_tx,
        })
    }
}

impl TrayHandle for TauriTray {
    fn try_recv_command(&self) -> Option<UiCommand> {
        self.cmd_rx.try_recv().ok()
    }

    fn update(&self, status: UiStatus) {
        apply_status(&self.app, &self.tray, &self.status_item, &status);
    }

    fn status_sender(&self) -> mpsc::Sender<UiStatus> {
        self.status_tx.clone()
    }
}

/// Builds the tray menu and icon. Must run on the main thread.
fn build_tray(
    app: &tauri::AppHandle,
    initial_label: &str,
    initial_state: &DictationState,
    cmd_tx: mpsc::Sender<UiCommand>,
    windows: Arc<dyn AppWindows>,
) -> anyhow::Result<(TrayIcon, MenuItem<tauri::Wry>)> {
    let status_item = MenuItem::with_id(app, MENU_STATUS, initial_label, false, None::<&str>)
        .map_err(|error| anyhow::anyhow!("failed to build tray status item: {error}"))?;
    let separator_top = PredefinedMenuItem::separator(app)
        .map_err(|error| anyhow::anyhow!("failed to build tray separator: {error}"))?;
    let test_recording = MenuItem::with_id(
        app,
        MENU_TEST_RECORDING,
        "Start Test Recording",
        true,
        None::<&str>,
    )
    .map_err(|error| anyhow::anyhow!("failed to build test-recording menu item: {error}"))?;
    let speak_text =
        MenuItem::with_id(app, MENU_SPEAK_TEXT, "Speak text...", true, None::<&str>)
            .map_err(|error| anyhow::anyhow!("failed to build speak-text menu item: {error}"))?;
    let settings = MenuItem::with_id(app, MENU_SETTINGS, "Open Settings", true, None::<&str>)
        .map_err(|error| anyhow::anyhow!("failed to build settings menu item: {error}"))?;
    let logs = MenuItem::with_id(app, MENU_LOGS, "Open Logs", true, None::<&str>)
        .map_err(|error| anyhow::anyhow!("failed to build logs menu item: {error}"))?;
    let diagnostics =
        MenuItem::with_id(app, MENU_DIAGNOSTICS, "Run Diagnostics", true, None::<&str>)
            .map_err(|error| anyhow::anyhow!("failed to build diagnostics menu item: {error}"))?;
    let separator_bottom = PredefinedMenuItem::separator(app)
        .map_err(|error| anyhow::anyhow!("failed to build tray separator: {error}"))?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)
        .map_err(|error| anyhow::anyhow!("failed to build quit menu item: {error}"))?;

    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &separator_top,
            &test_recording,
            &speak_text,
            &settings,
            &logs,
            &diagnostics,
            &separator_bottom,
            &quit,
        ],
    )
    .map_err(|error| anyhow::anyhow!("failed to build tray menu: {error}"))?;

    let icon =
        tauri::image::Image::new_owned(icon_rgba_for_state(initial_state), ICON_SIZE, ICON_SIZE);

    let tray = TrayIconBuilder::new()
        .icon(icon)
        .tooltip(initial_label)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(
            move |_app, event: MenuEvent| match map_menu_event(event.id().as_ref()) {
                Some(MenuAction::OpenMain) => windows.open_main(),
                Some(MenuAction::OpenSettings) => windows.open_settings(),
                Some(MenuAction::Command(command)) => {
                    let _ = cmd_tx.send(command);
                }
                None => {}
            },
        )
        .build(app)
        .map_err(|error| anyhow::anyhow!("failed to build tray icon: {error}"))?;

    Ok((tray, status_item))
}

/// Applies a status update to the tray icon/tooltip/menu text (dispatched to
/// the main thread) and enqueues the HUD notification (the HUD runs on its own
/// worker thread). Safe to call concurrently from the run loop and the pump.
fn apply_status(
    app: &tauri::AppHandle,
    tray: &TrayIcon,
    status_item: &MenuItem<tauri::Wry>,
    status: &UiStatus,
) {
    let label = status.tray_label();
    let icon_state = status.state.clone();
    let tray = tray.clone();
    let status_item = status_item.clone();
    let main_thread_label = label.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        let image =
            tauri::image::Image::new_owned(icon_rgba_for_state(&icon_state), ICON_SIZE, ICON_SIZE);
        if let Err(error) = tray.set_icon(Some(image)) {
            tracing::warn!(%error, "failed to update tray icon");
        }
        if let Err(error) = tray.set_tooltip(Some(&main_thread_label)) {
            tracing::warn!(%error, "failed to update tray tooltip");
        }
        if let Err(error) = status_item.set_text(&main_thread_label) {
            tracing::warn!(%error, "failed to update tray status menu item text");
        }
    }) {
        tracing::warn!(%error, "failed to dispatch tray status update to main thread");
    }

    hud::show(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_menu_event_routes_speak_text_to_open_main() {
        assert_eq!(map_menu_event(MENU_SPEAK_TEXT), Some(MenuAction::OpenMain));
    }

    #[test]
    fn map_menu_event_routes_settings_to_open_settings() {
        assert_eq!(
            map_menu_event(MENU_SETTINGS),
            Some(MenuAction::OpenSettings)
        );
    }

    #[test]
    fn map_menu_event_routes_commands() {
        assert_eq!(
            map_menu_event(MENU_TEST_RECORDING),
            Some(MenuAction::Command(UiCommand::StartTestRecording))
        );
        assert_eq!(
            map_menu_event(MENU_LOGS),
            Some(MenuAction::Command(UiCommand::OpenLogs))
        );
        assert_eq!(
            map_menu_event(MENU_DIAGNOSTICS),
            Some(MenuAction::Command(UiCommand::RunDiagnostics))
        );
        assert_eq!(
            map_menu_event(MENU_QUIT),
            Some(MenuAction::Command(UiCommand::Quit))
        );
    }

    #[test]
    fn map_menu_event_ignores_unknown_and_status_ids() {
        assert_eq!(map_menu_event(MENU_STATUS), None);
        assert_eq!(map_menu_event("bogus"), None);
    }
}
