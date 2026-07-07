//! The two on-demand Linux windows (Settings, Speak Text), rendered by an iced
//! daemon that owns the process main thread.
//!
//! iced 0.14 builds its winit event loop on the calling thread, and winit 0.30
//! refuses off-main-thread creation on Linux, so [`run_window_daemon`] must be
//! called from the main thread. It starts with zero windows and opens or closes
//! them in response to [`WindowEvent`]s delivered over a channel from the tray.
//! The daemon does not exit when all windows close; it exits only when it
//! receives [`WindowEvent::Exit`] (the app is shutting down).

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use iced::widget::{button, column, container, row, text, text_editor};
use iced::{window, Element, Length, Size, Subscription, Task};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::linux_tray::WindowEvent;
use crate::tray_common::UiCommand;
use crate::UiStatus;

/// Static rows shown in the Settings window, ported verbatim from the previous
/// GTK settings window.
const SETTINGS_ROWS: [&str; 5] = [
    "Hotkeys: Control-M or keyboard dictation key (KDE GlobalShortcuts portal)",
    "Insertion: Wayland RemoteDesktop portal paste",
    "Transcription: Codex auth from ~/.codex/auth.json",
    "Timeout: default runtime timeout",
    "Debug logs: set RUST_LOG before launching",
];

/// Static information the Settings window displays. Cloned into the daemon state
/// at boot.
#[derive(Debug, Clone)]
pub struct SettingsInfo {
    pub log_path: PathBuf,
}

impl SettingsInfo {
    pub fn new(log_path: PathBuf) -> Self {
        Self { log_path }
    }
}

/// Handoff slot for the [`WindowEvent`] receiver.
///
/// `Subscription::run` takes a bare `fn` pointer that cannot capture state, so
/// the receiver is parked here and taken once inside [`window_event_stream`].
static EVENT_RX: OnceLock<Mutex<Option<UnboundedReceiver<WindowEvent>>>> = OnceLock::new();

/// Runs the iced window daemon on the current (main) thread until the app sends
/// [`WindowEvent::Exit`]. Returns an error if iced cannot start (e.g. no display),
/// which the caller treats as a soft failure and degrades to tray-only/headless.
pub fn run_window_daemon(
    events: UnboundedReceiver<WindowEvent>,
    command_tx: std::sync::mpsc::Sender<UiCommand>,
    info: SettingsInfo,
) -> iced::Result {
    // Park the receiver for the subscription. Runs once per process; a second
    // call (which never happens in normal operation) would keep the first.
    let _ = EVENT_RX.set(Mutex::new(Some(events)));

    iced::daemon(
        move || WindowState::new(command_tx.clone(), info.clone()),
        WindowState::update,
        WindowState::view,
    )
    .title(WindowState::title)
    .subscription(WindowState::subscription)
    .run()
}

#[derive(Debug, Clone)]
enum Message {
    /// A signal from the tray/app.
    Incoming(WindowEvent),
    /// The Settings window finished opening.
    SettingsOpened(window::Id),
    /// The Speak Text window finished opening.
    SpeakOpened(window::Id),
    /// A window was closed (by the user or by us); prune its stored id.
    WindowClosed(window::Id),
    /// The user edited the Speak Text editor.
    Edit(text_editor::Action),
    /// The user pressed Generate in the Speak Text window.
    Generate,
    /// The user pressed Play in the Speak Text window.
    Play,
    /// The user pressed Close in the Speak Text window.
    CloseSpeak,
}

struct WindowState {
    settings_win: Option<window::Id>,
    speak_win: Option<window::Id>,
    status: UiStatus,
    content: text_editor::Content,
    command_tx: std::sync::mpsc::Sender<UiCommand>,
    info: SettingsInfo,
}

impl WindowState {
    fn new(command_tx: std::sync::mpsc::Sender<UiCommand>, info: SettingsInfo) -> Self {
        Self {
            settings_win: None,
            speak_win: None,
            status: UiStatus::idle(),
            content: text_editor::Content::new(),
            command_tx,
            info,
        }
    }

    fn title(&self, window: window::Id) -> String {
        if Some(window) == self.speak_win {
            "Speak Text".to_string()
        } else {
            "Codex Voice Settings".to_string()
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Incoming(WindowEvent::OpenSettings) => self.open_settings(),
            Message::Incoming(WindowEvent::OpenSpeakText) => self.open_speak(),
            Message::Incoming(WindowEvent::Status(status)) => {
                self.status = status;
                Task::none()
            }
            Message::Incoming(WindowEvent::Exit) => iced::exit(),
            Message::SettingsOpened(id) => {
                self.settings_win = Some(id);
                Task::none()
            }
            Message::SpeakOpened(id) => {
                self.speak_win = Some(id);
                Task::none()
            }
            Message::WindowClosed(id) => {
                // Tolerate a possibly-missed final close event on Wayland
                // (iced #3229): the next open call simply mints a fresh window.
                if self.settings_win == Some(id) {
                    self.settings_win = None;
                }
                if self.speak_win == Some(id) {
                    self.speak_win = None;
                }
                Task::none()
            }
            Message::Edit(action) => {
                self.content.perform(action);
                Task::none()
            }
            Message::Generate => {
                let _ = self
                    .command_tx
                    .send(UiCommand::SpeakText(self.content.text()));
                Task::none()
            }
            Message::Play => {
                let _ = self.command_tx.send(UiCommand::PlayLastSpeech);
                Task::none()
            }
            Message::CloseSpeak => match self.speak_win {
                Some(id) => window::close(id),
                None => Task::none(),
            },
        }
    }

    fn open_settings(&mut self) -> Task<Message> {
        if let Some(id) = self.settings_win {
            return window::gain_focus(id);
        }
        let (id, task) = window::open(window_settings(460.0, 320.0));
        // Record the id eagerly so a rapid second request focuses rather than
        // opening a duplicate; the task result re-confirms the same id.
        self.settings_win = Some(id);
        task.map(Message::SettingsOpened)
    }

    fn open_speak(&mut self) -> Task<Message> {
        if let Some(id) = self.speak_win {
            return window::gain_focus(id);
        }
        let (id, task) = window::open(window_settings(520.0, 360.0));
        self.speak_win = Some(id);
        task.map(Message::SpeakOpened)
    }

    fn view(&self, window: window::Id) -> Element<'_, Message> {
        if Some(window) == self.speak_win {
            self.speak_view()
        } else {
            self.settings_view()
        }
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let mut col = column![
            text("Codex Voice").size(20),
            text(format!("Status: {}", self.status.message)),
        ]
        .spacing(8)
        .padding(18);
        for row in SETTINGS_ROWS {
            col = col.push(text(row));
        }
        col = col.push(text(format!("Log file: {}", self.info.log_path.display())));
        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn speak_view(&self) -> Element<'_, Message> {
        let editor = text_editor(&self.content)
            .on_action(Message::Edit)
            .height(Length::Fill);
        let buttons = row![
            button("Generate").on_press(Message::Generate),
            button("Play").on_press(Message::Play),
            button("Close").on_press(Message::CloseSpeak),
        ]
        .spacing(8);
        container(column![editor, buttons].spacing(10).padding(14))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(window_event_stream),
            window::close_events().map(Message::WindowClosed),
        ])
    }
}

fn window_settings(width: f32, height: f32) -> window::Settings {
    window::Settings {
        size: Size::new(width, height),
        min_size: Some(Size::new(width, height)),
        ..Default::default()
    }
}

/// Bridges the app's [`WindowEvent`] receiver into an iced subscription stream.
/// Takes the parked receiver once; subsequent (never-expected) invocations get
/// an empty stream.
fn window_event_stream() -> impl iced::futures::Stream<Item = Message> {
    let receiver = EVENT_RX
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|mut guard| guard.take()));

    iced::futures::stream::unfold(receiver, |receiver| async move {
        let mut receiver = receiver?;
        receiver
            .recv()
            .await
            .map(|event| (Message::Incoming(event), Some(receiver)))
    })
}
