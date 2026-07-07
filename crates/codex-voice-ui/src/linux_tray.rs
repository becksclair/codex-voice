//! Linux status tray backed by ksni (StatusNotifierItem over D-Bus).
//!
//! KDE Plasma — the primary target desktop — speaks the StatusNotifierItem
//! protocol natively, so the tray is a pure-Rust D-Bus service with no GTK
//! anywhere. The two on-demand windows (Settings, Speak Text) are rendered by
//! the iced daemon in [`crate::linux_windows`], which must own the process main
//! thread; this module only signals it through a [`WindowEvent`] channel.
//!
//! ## Threading
//!
//! [`StatusTray::start`] keeps its synchronous, blocking-until-ready contract by
//! spawning a dedicated thread that owns a small multi-threaded tokio runtime.
//! ksni spawns its service loop with `tokio::spawn`, so the runtime must keep
//! driving that task independently of status updates — a `current_thread`
//! runtime would only poll the service while a `block_on` was active and the
//! menu would freeze between updates. One worker thread is enough.

use codex_voice_core::DictationState;
use std::{
    collections::HashMap,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::tray_common::{icon_rgba_for_state, UiCommand, UiError, ICON_SIZE};
use crate::UiStatus;

/// Signals from the tray/app to the iced window daemon running on the main
/// thread. The daemon opens or focuses windows, mirrors live status into an
/// open Settings window, and exits when the app shuts down.
#[derive(Debug, Clone)]
pub enum WindowEvent {
    /// Open (or focus, if already open) the Settings window.
    OpenSettings,
    /// Open (or focus, if already open) the Speak Text window.
    OpenSpeakText,
    /// Refresh the live status message shown by an open Settings window.
    Status(String),
    /// Shut the daemon down; the process is exiting.
    Exit,
}

/// Per-platform tray configuration. Not part of the frozen `StatusTray` method
/// surface, so it carries the Linux-specific channel wiring the tray and the
/// iced window daemon share.
pub struct LinuxUiConfig {
    /// Signals to the iced window daemon (open/focus windows, live status).
    pub window_tx: UnboundedSender<WindowEvent>,
    /// Sender the ksni menu closures use to enqueue [`UiCommand`]s.
    pub command_tx: Sender<UiCommand>,
    /// Receiver drained by the app run-loop via [`StatusTray::try_recv_command`].
    pub command_rx: Receiver<UiCommand>,
}

pub struct StatusTray {
    status_tx: Sender<UiStatus>,
    command_rx: Receiver<UiCommand>,
    _thread: thread::JoinHandle<()>,
}

impl StatusTray {
    pub fn start(initial: UiStatus, config: LinuxUiConfig) -> Result<Self, UiError> {
        let (status_tx, status_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let LinuxUiConfig {
            window_tx,
            command_tx,
            command_rx,
        } = config;

        let thread = thread::Builder::new()
            .name("codex-voice-tray".to_string())
            .spawn(move || {
                run_tray(initial, status_rx, command_tx, window_tx, ready_tx);
            })
            .map_err(|error| UiError::TrayInit(format!("failed to spawn tray thread: {error}")))?;

        ready_rx
            .recv()
            .map_err(|_| UiError::EventLoop("tray thread stopped during startup".to_string()))??;

        Ok(Self {
            status_tx,
            command_rx,
            _thread: thread,
        })
    }

    pub fn update(&self, status: UiStatus) {
        let _ = self.status_tx.send(status);
    }

    pub fn try_recv_command(&self) -> Option<UiCommand> {
        self.command_rx.try_recv().ok()
    }

    /// Returns a cloneable sender for updating the tray status.
    pub fn status_sender(&self) -> Sender<UiStatus> {
        self.status_tx.clone()
    }
}

/// The tray struct ksni renders. Its fields are the entire tray state; `menu()`
/// and the icon/title/tooltip getters are pure functions of them. Menu closures
/// enqueue work onto the app (via `command_tx`) or the window daemon (via
/// `window_tx`) rather than mutating the tray directly.
struct KsniTray {
    status: UiStatus,
    command_tx: Sender<UiCommand>,
    window_tx: UnboundedSender<WindowEvent>,
    icons: HashMap<DictationState, ksni::Icon>,
}

impl KsniTray {
    fn current_icon(&self) -> ksni::Icon {
        icon_for_state(&self.icons, &self.status.state)
    }
}

impl ksni::Tray for KsniTray {
    // Preserve appindicator-style behavior: a left click opens the menu rather
    // than firing a separate activate action.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "codex-voice".to_string()
    }

    fn title(&self) -> String {
        self.status.title().to_string()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.current_icon()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Codex Voice".to_string(),
            description: self.status.message.clone(),
            ..Default::default()
        }
    }

    // Survive a plasmashell / StatusNotifierWatcher restart: returning `true`
    // keeps the service alive so it re-registers when the watcher returns.
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }

    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        // Each closure owns its own sender clones; `menu()` may be re-rendered
        // any time ksni needs the layout.
        let command_test = self.command_tx.clone();
        let command_logs = self.command_tx.clone();
        let command_diagnostics = self.command_tx.clone();
        let command_quit = self.command_tx.clone();
        let window_settings = self.window_tx.clone();
        let window_speak = self.window_tx.clone();

        vec![
            StandardItem {
                label: self.status.tray_label(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Start Test Recording".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = command_test.send(UiCommand::StartTestRecording);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Speak text...".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = window_speak.send(WindowEvent::OpenSpeakText);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Settings".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = window_settings.send(WindowEvent::OpenSettings);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Logs".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = command_logs.send(UiCommand::OpenLogs);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Run Diagnostics".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = command_diagnostics.send(UiCommand::RunDiagnostics);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(move |_: &mut Self| {
                    let _ = command_quit.send(UiCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn run_tray(
    initial: UiStatus,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    window_tx: UnboundedSender<WindowEvent>,
    ready_tx: Sender<Result<(), UiError>>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready_tx.send(Err(UiError::TrayInit(format!(
                "failed to build tray runtime: {error}"
            ))));
            return;
        }
    };

    let icons = build_ksni_icon_cache();
    let tray = KsniTray {
        status: initial,
        command_tx,
        window_tx: window_tx.clone(),
        icons,
    };

    let handle = match runtime.block_on(async { ksni::TrayMethods::spawn(tray).await }) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = ready_tx.send(Err(map_spawn_error(error)));
            return;
        }
    };

    let _ = ready_tx.send(Ok(()));

    let mut hud = HudWindow::new();
    // Blocking recv on the tray thread; the ksni service keeps running on the
    // runtime's worker thread regardless. Each status update refreshes the tray
    // via the D-Bus-triggering handle update, drives the notify-send HUD, and
    // mirrors live status into an open Settings window.
    while let Ok(status) = status_rx.recv() {
        hud.update(&status);
        let _ = window_tx.send(WindowEvent::Status(status.message.clone()));
        let updated = runtime.block_on(handle.update(|tray: &mut KsniTray| {
            tray.status = status.clone();
        }));
        if updated.is_none() {
            // The ksni service ended (D-Bus gone, watcher shut down for good).
            // Surface it once and stop forwarding instead of silently eating
            // every future status update.
            eprintln!("codex-voice tray service ended; tray status updates stopped");
            break;
        }
    }

    // The app dropped the status sender: it is shutting down. Tear the service
    // down cleanly so the icon disappears promptly.
    runtime.block_on(handle.shutdown());
}

/// Maps a ksni spawn failure onto the friendly [`UiError::TrayInit`]. The ksni
/// `Error` enum is `#[non_exhaustive]`, so the wildcard is required.
fn map_spawn_error(error: ksni::Error) -> UiError {
    let detail = match error {
        ksni::Error::Watcher(_) => {
            "no StatusNotifierWatcher is available (is a system tray running?)".to_string()
        }
        ksni::Error::WontShow => {
            "the tray registered but no StatusNotifierHost will display it".to_string()
        }
        ksni::Error::Dbus(err) => format!("D-Bus error: {err}"),
        other => format!("{other:?}"),
    };
    UiError::TrayInit(format!("failed to start status tray: {detail}"))
}

/// Builds the per-state ksni icon cache from the shared RGBA pixel source.
fn build_ksni_icon_cache() -> HashMap<DictationState, ksni::Icon> {
    use codex_voice_core::DictationState::*;
    let mut cache = HashMap::new();
    for state in [
        Idle,
        Recording,
        Transcribing,
        Inserting,
        Error(String::new()),
    ] {
        let icon = ksni_icon_for_state(&state);
        cache.insert(state, icon);
    }
    cache
}

fn icon_for_state(
    cache: &HashMap<DictationState, ksni::Icon>,
    state: &DictationState,
) -> ksni::Icon {
    let lookup = match state {
        DictationState::Error(_) => DictationState::Error(String::new()),
        _ => state.clone(),
    };
    cache
        .get(&lookup)
        .cloned()
        .or_else(|| cache.get(&DictationState::Error(String::new())).cloned())
        .expect("icon cache contains all states")
}

/// Converts the shared 32x32 RGBA circle into a ksni [`ksni::Icon`].
///
/// ksni icons are ARGB32 in network byte order, whereas the shared source is
/// RGBA. Rotating each 4-byte pixel right by one turns `[r, g, b, a]` into
/// `[a, r, g, b]`.
fn ksni_icon_for_state(state: &DictationState) -> ksni::Icon {
    let mut data = icon_rgba_for_state(state);
    for pixel in data.chunks_exact_mut(4) {
        pixel.rotate_right(1);
    }
    ksni::Icon {
        width: ICON_SIZE as i32,
        height: ICON_SIZE as i32,
        data,
    }
}

/// Transient desktop notifications via `notify-send` (no GTK dependency). This
/// is unchanged from the previous tray implementation.
struct HudWindow {
    replace_id: Option<String>,
    last_message: Option<String>,
    available: bool,
}

impl HudWindow {
    fn new() -> Self {
        Self {
            replace_id: None,
            last_message: None,
            available: Command::new("notify-send")
                .arg("--version")
                .output()
                .is_ok(),
        }
    }

    fn update(&mut self, status: &UiStatus) {
        if !self.available || status.state == DictationState::Idle {
            self.last_message = None;
            return;
        }
        if self.last_message.as_deref() == Some(status.message.as_str()) {
            return;
        }

        let timeout_ms = match status.state {
            DictationState::Recording => "60000",
            DictationState::Error(_) => "8000",
            _ => "2500",
        };
        let urgency = match status.state {
            DictationState::Error(_) => "critical",
            _ => "low",
        };
        let mut command = Command::new("notify-send");
        command
            .arg("--print-id")
            .arg("--transient")
            .arg("--app-name=Codex Voice")
            .arg("--category=status")
            .arg("--urgency")
            .arg(urgency)
            .arg("--expire-time")
            .arg(timeout_ms);
        if let Some(replace_id) = &self.replace_id {
            command.arg("--replace-id").arg(replace_id);
        }
        let output = command.arg("Codex Voice").arg(&status.message).output();
        match output {
            Ok(output) if output.status.success() => {
                let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !id.is_empty() {
                    self.replace_id = Some(id);
                }
                self.last_message = Some(status.message.clone());
            }
            Ok(_) | Err(_) => {
                self.available = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ksni_icon_reorders_rgba_to_argb() {
        // The center pixel of the Idle icon is opaque idle-gray. In RGBA that is
        // [0x5c, 0x66, 0x70, 0xff]; ksni wants ARGB32 network byte order, i.e.
        // [0xff, 0x5c, 0x66, 0x70].
        let icon = ksni_icon_for_state(&DictationState::Idle);
        assert_eq!(icon.width, ICON_SIZE as i32);
        assert_eq!(icon.height, ICON_SIZE as i32);
        assert_eq!(icon.data.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);

        let center = (((ICON_SIZE / 2) * ICON_SIZE + (ICON_SIZE / 2)) * 4) as usize;
        assert_eq!(
            &icon.data[center..center + 4],
            &[0xff, 0x5c, 0x66, 0x70],
            "center pixel should be ARGB idle-gray"
        );

        // A transparent corner: RGBA [r, g, b, 0x00] -> ARGB [0x00, r, g, b].
        assert_eq!(
            &icon.data[0..4],
            &[0x00, 0x5c, 0x66, 0x70],
            "corner pixel should be fully transparent ARGB"
        );
    }
}
