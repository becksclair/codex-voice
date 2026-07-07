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
pub struct MacOSUiConfig {
    pub log_path: PathBuf,
}

pub struct StatusTray {
    status_tx: Sender<UiStatus>,
    command_rx: Receiver<UiCommand>,
    _thread: thread::JoinHandle<()>,
}

impl StatusTray {
    pub fn start(initial: UiStatus, config: MacOSUiConfig) -> Result<Self, UiError> {
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

    pub fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus> {
        self.status_tx.clone()
    }
}

fn run_tray(
    initial: UiStatus,
    config: MacOSUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), UiError>>,
) {
    let result = initialize_tray(initial, config, status_rx, command_tx, ready_tx.clone());
    if let Err(error) = result {
        let _ = ready_tx.send(Err(error.clone()));
        eprintln!("codex-voice tray stopped: {error}");
    }
}

fn initialize_tray(
    initial: UiStatus,
    config: MacOSUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), UiError>>,
) -> Result<(), UiError> {
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

    let _ = ready_tx.send(Ok(()));
    let mut current_status = initial;
    let mut hud = HudNotifier::new();

    loop {
        while let Ok(status) = status_rx.try_recv() {
            current_status = status;
            status_item.set_text(current_status.tray_label());
            tray.set_title(Some(current_status.title()));
            tray.set_icon(Some(icon_for_state(&icons, &current_status.state)))
                .map_err(|error| UiError::Icon(format!("failed to update tray icon: {error}")))?;
            hud.update(&current_status);
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id().as_ref() {
                MENU_TEST_RECORDING => {
                    let _ = command_tx.send(UiCommand::StartTestRecording);
                }
                MENU_SPEAK_TEXT => {
                    show_speak_text_dialog(command_tx.clone());
                }
                MENU_SETTINGS => {
                    show_settings_dialog(&config, &current_status);
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

struct HudNotifier {
    last_message: Option<String>,
    available: bool,
}

impl HudNotifier {
    fn new() -> Self {
        Self {
            last_message: None,
            available: Command::new("osascript").arg("--version").output().is_ok(),
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

        let title = status.title();

        let script = format!(
            "display notification \"{}\" with title \"Codex Voice – {}\" sound name \"Funk\"",
            status.message.replace("\"", "\\\""),
            title
        );

        let output = Command::new("osascript").arg("-e").arg(&script).output();

        match output {
            Ok(out) if out.status.success() => {
                self.last_message = Some(status.message.clone());
            }
            Ok(_) | Err(_) => {
                // osascript failed — disable further attempts to avoid repeated spawns
                self.available = false;
            }
        }
    }
}

fn show_settings_dialog(config: &MacOSUiConfig, status: &UiStatus) {
    let text = format!(
        "Status: {}\n\nHotkeys: Control-M (global-hotkey / Carbon)\nInsertion: Accessibility selected-text replacement, fallback to clipboard + CGEvent paste\nTranscription: Codex auth from ~/.codex/auth.json\nTimeout: default runtime timeout\nDebug logs: set RUST_LOG before launching\n\nLog file: {}",
        status.message,
        config.log_path.display()
    );

    let script = format!(
        "tell app \"System Events\" to display dialog \"{}\" buttons {{\"OK\"}} default button \"OK\" with title \"Codex Voice Settings\"",
        text.replace("\"", "\\\"")
    );

    if let Ok(mut child) = Command::new("osascript").arg("-e").arg(&script).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn show_speak_text_dialog(command_tx: Sender<UiCommand>) {
    thread::spawn(move || {
        let script = "display dialog \"Paste text to speak:\" default answer \"\" buttons {\"Cancel\", \"Play\", \"Generate\"} default button \"Generate\" with title \"Speak Text\"";
        let output = Command::new("osascript").arg("-e").arg(script).output();
        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("button returned:Play") {
                let _ = command_tx.send(UiCommand::PlayLastSpeech);
            } else if stdout.contains("button returned:Generate") {
                let text = stdout
                    .split("text returned:")
                    .nth(1)
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                let _ = command_tx.send(UiCommand::SpeakText(text));
            }
        }
    });
}
