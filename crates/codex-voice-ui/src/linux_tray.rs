use codex_voice_core::DictationState;
use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder,
};

use crate::tray_common::{
    build_icon_cache, icon_for_state, UiCommand, UiError, MENU_DIAGNOSTICS, MENU_LOGS, MENU_QUIT,
    MENU_SETTINGS, MENU_SPEAK_TEXT, MENU_STATUS, MENU_TEST_RECORDING,
};
use crate::UiStatus;

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
    pub fn start(initial: UiStatus, config: LinuxUiConfig) -> Result<Self, UiError> {
        let (status_tx, status_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread = thread::spawn(move || {
            run_tray(initial, config, status_rx, command_tx, ready_tx);
        });

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
    pub fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus> {
        self.status_tx.clone()
    }
}

fn run_tray(
    initial: UiStatus,
    config: LinuxUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), UiError>>,
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
    ready_tx: Sender<Result<(), UiError>>,
) -> Result<(), UiError> {
    gtk::init().map_err(|error| UiError::TrayInit(format!("failed to initialize GTK: {error}")))?;

    let menu = Menu::new();
    let status_item = MenuItem::with_id(MENU_STATUS, initial.tray_label(), false, None);
    let test_recording_item =
        MenuItem::with_id(MENU_TEST_RECORDING, "Start Test Recording", true, None);
    let speak_text_item = MenuItem::with_id(MENU_SPEAK_TEXT, "Speak text...", true, None);
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
        &speak_text_item,
        &settings_item,
        &logs_item,
        &diagnostics_item,
        &utility_separator,
        &quit_item,
    ])
    .map_err(|error| UiError::TrayInit(format!("failed to build tray menu: {error}")))?;

    let icons = build_icon_cache()
        .map_err(|e| UiError::Icon(format!("failed to build icon cache: {e}")))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon_for_state(&icons, &initial.state))
        .with_title(initial.title())
        .with_tooltip("Codex Voice")
        .build()
        .map_err(|error| UiError::TrayInit(format!("failed to create tray icon: {error}")))?;
    let mut hud = HudWindow::new();
    let settings = SettingsWindow::new(&initial, &config);
    let speak_dialog = SpeakTextDialog::new(command_tx.clone());

    let _ = ready_tx.send(Ok(()));

    loop {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }

        while let Ok(status) = status_rx.try_recv() {
            status_item.set_text(status.tray_label());
            tray.set_title(Some(status.title()));
            tray.set_icon(Some(icon_for_state(&icons, &status.state)))
                .map_err(|error| UiError::Icon(format!("failed to update tray icon: {error}")))?;
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
                MENU_SPEAK_TEXT => {
                    speak_dialog.show();
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

struct SpeakTextDialog {
    window: gtk::Window,
}

impl SpeakTextDialog {
    fn new(command_tx: Sender<UiCommand>) -> Self {
        use gtk::prelude::*;

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Speak Text");
        window.set_default_size(520, 360);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(14);
        root.set_margin_bottom(14);
        root.set_margin_start(14);
        root.set_margin_end(14);

        let scroller = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let text_view = gtk::TextView::new();
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        scroller.add(&text_view);
        root.pack_start(&scroller, true, true, 0);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let generate = gtk::Button::with_label("Generate");
        let play = gtk::Button::with_label("Play");
        let close = gtk::Button::with_label("Close");
        buttons.pack_start(&generate, false, false, 0);
        buttons.pack_start(&play, false, false, 0);
        buttons.pack_end(&close, false, false, 0);
        root.pack_start(&buttons, false, false, 0);

        let buffer = text_view.buffer().expect("TextView has a buffer");
        let tx = command_tx.clone();
        generate.connect_clicked(move |_| {
            let start = buffer.start_iter();
            let end = buffer.end_iter();
            let text = buffer
                .text(&start, &end, true)
                .map(|value| value.to_string())
                .unwrap_or_default();
            let _ = tx.send(UiCommand::SpeakText(text));
        });

        let tx = command_tx.clone();
        play.connect_clicked(move |_| {
            let _ = tx.send(UiCommand::PlayLastSpeech);
        });

        let close_window = window.clone();
        close.connect_clicked(move |_| {
            close_window.hide();
        });

        window.add(&root);
        Self { window }
    }

    fn show(&self) {
        use gtk::prelude::*;

        self.window.show_all();
        self.window.present();
    }
}
