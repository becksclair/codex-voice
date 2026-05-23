use codex_voice_core::DictationState;
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};

use crate::UiStatus;

const MENU_STATUS: &str = "status";
const MENU_TEST_RECORDING: &str = "test-recording";
const MENU_SETTINGS: &str = "settings";
const MENU_LOGS: &str = "logs";
const MENU_DIAGNOSTICS: &str = "diagnostics";
const MENU_QUIT: &str = "quit";
const ICON_SIZE: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    StartTestRecording,
    OpenLogs,
    RunDiagnostics,
    Quit,
}

#[derive(Debug, Clone)]
pub struct LinuxUiConfig {
    pub log_path: PathBuf,
}

pub struct StatusTray {
    status_tx: Sender<UiStatus>,
    command_rx: Receiver<UiCommand>,
    _thread: thread::JoinHandle<()>,
}

impl StatusTray {
    pub fn start(initial: UiStatus, config: LinuxUiConfig) -> Result<Self, String> {
        let (status_tx, status_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread = thread::spawn(move || {
            run_tray(initial, config, status_rx, command_tx, ready_tx);
        });

        ready_rx
            .recv()
            .map_err(|_| "tray thread stopped during startup".to_string())??;

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
    pub fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus> {
        self.status_tx.clone()
    }
}

fn run_tray(
    initial: UiStatus,
    config: LinuxUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), String>>,
) {
    let result = initialize_tray(initial, config, status_rx, command_tx, ready_tx.clone());
    if let Err(error) = result {
        let _ = ready_tx.send(Err(error.clone()));
        // If startup has already been reported, the app can only learn about
        // later tray-loop failures through logs in a future UI milestone.
        eprintln!("codex-voice tray stopped: {error}");
    }
}

fn initialize_tray(
    initial: UiStatus,
    config: LinuxUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), String>>,
) -> Result<(), String> {
    gtk::init().map_err(|error| format!("failed to initialize GTK: {error}"))?;

    let menu = Menu::new();
    let status_item = MenuItem::with_id(MENU_STATUS, initial.tray_label(), false, None);
    let test_recording_item =
        MenuItem::with_id(MENU_TEST_RECORDING, "Start Test Recording", true, None);
    let settings_item = MenuItem::with_id(MENU_SETTINGS, "Open Settings", true, None);
    let logs_item = MenuItem::with_id(MENU_LOGS, "Open Logs", true, None);
    let diagnostics_item = MenuItem::with_id(MENU_DIAGNOSTICS, "Run Diagnostics", true, None);
    let quit_item = MenuItem::with_id(MENU_QUIT, "Quit", true, None);
    let separator = PredefinedMenuItem::separator();
    let utility_separator = PredefinedMenuItem::separator();
    menu.append_items(&[
        &status_item,
        &separator,
        &test_recording_item,
        &settings_item,
        &logs_item,
        &diagnostics_item,
        &utility_separator,
        &quit_item,
    ])
    .map_err(|error| format!("failed to build tray menu: {error}"))?;

    let icons = build_icon_cache().map_err(|e| format!("failed to build icon cache: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon_for_state(&icons, &initial.state))
        .with_title(initial.title())
        .with_tooltip("Codex Voice")
        .build()
        .map_err(|error| format!("failed to create tray icon: {error}"))?;
    let mut hud = HudWindow::new();
    let settings = SettingsWindow::new(&initial, &config);

    let _ = ready_tx.send(Ok(()));

    loop {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }

        while let Ok(status) = status_rx.try_recv() {
            status_item.set_text(status.tray_label());
            tray.set_title(Some(status.title()));
            tray.set_icon(Some(icon_for_state(&icons, &status.state)))
                .map_err(|error| format!("failed to update tray icon: {error}"))?;
            hud.update(&status);
            settings.update(&status);
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id().as_ref() {
                MENU_TEST_RECORDING => {
                    let _ = command_tx.send(UiCommand::StartTestRecording);
                }
                MENU_SETTINGS => {
                    settings.show();
                }
                MENU_LOGS => {
                    let _ = command_tx.send(UiCommand::OpenLogs);
                }
                MENU_DIAGNOSTICS => {
                    let _ = command_tx.send(UiCommand::RunDiagnostics);
                }
                MENU_QUIT => {
                    let _ = command_tx.send(UiCommand::Quit);
                    return Ok(());
                }
                _ => {}
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}

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

struct SettingsWindow {
    window: gtk::Window,
    status_label: gtk::Label,
}

impl SettingsWindow {
    fn new(initial: &UiStatus, config: &LinuxUiConfig) -> Self {
        use gtk::prelude::*;

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Codex Voice Settings");
        window.set_default_size(460, 280);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);

        let heading = gtk::Label::new(Some("Codex Voice"));
        heading.set_xalign(0.0);
        heading.set_markup("<b>Codex Voice</b>");
        root.pack_start(&heading, false, false, 0);

        let status_label = gtk::Label::new(None);
        status_label.set_xalign(0.0);
        status_label.set_selectable(true);
        root.pack_start(&status_label, false, false, 0);

        for row in [
            "Hotkeys: Control-M or keyboard dictation key (KDE GlobalShortcuts portal)",
            "Insertion: Wayland RemoteDesktop portal paste",
            "Transcription: Codex auth from ~/.codex/auth.json",
            "Timeout: default runtime timeout",
            "Debug logs: set RUST_LOG before launching",
        ] {
            let label = gtk::Label::new(Some(row));
            label.set_xalign(0.0);
            label.set_selectable(true);
            root.pack_start(&label, false, false, 0);
        }

        let log_label = gtk::Label::new(Some(&format!("Log file: {}", config.log_path.display())));
        log_label.set_xalign(0.0);
        log_label.set_selectable(true);
        root.pack_start(&log_label, false, false, 0);

        window.add(&root);
        let settings = Self {
            window,
            status_label,
        };
        settings.update(initial);
        settings
    }

    fn show(&self) {
        use gtk::prelude::*;

        self.window.show_all();
        self.window.present();
    }

    fn update(&self, status: &UiStatus) {
        use gtk::prelude::*;

        self.status_label
            .set_label(&format!("Status: {}", status.message));
    }
}

fn build_icon_cache() -> Result<HashMap<DictationState, Icon>, String> {
    use codex_voice_core::DictationState::*;
    let mut cache = HashMap::new();
    for state in [
        Idle,
        Recording,
        Transcribing,
        Inserting,
        Error(String::new()),
    ] {
        let icon = build_icon_for_state(&state)?;
        cache.insert(state, icon);
    }
    Ok(cache)
}

fn icon_for_state(cache: &HashMap<DictationState, Icon>, state: &DictationState) -> Icon {
    // Error states with different inner strings share the same icon.
    cache
        .get(state)
        .cloned()
        .or_else(|| cache.get(&DictationState::Error(String::new())).cloned())
        .expect("icon cache contains all states")
}

fn build_icon_for_state(state: &DictationState) -> Result<Icon, String> {
    let color = match state {
        DictationState::Idle => [0x5c, 0x66, 0x70, 0xff],
        DictationState::Recording => [0xdb, 0x36, 0x36, 0xff],
        DictationState::Transcribing => [0x2b, 0x7f, 0xd3, 0xff],
        DictationState::Inserting => [0xf2, 0xb8, 0x4b, 0xff],
        DictationState::Error(_) => [0xcc, 0x24, 0x1d, 0xff],
    };

    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    let radius = (ICON_SIZE as f32) / 2.0 - 2.0;
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let alpha = if (dx * dx + dy * dy).sqrt() <= radius {
                color[3]
            } else {
                0
            };
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }

    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE)
        .map_err(|error| format!("failed to build tray icon: {error}"))
}
