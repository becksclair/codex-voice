use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use async_trait::async_trait;
use codex_voice_core::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, TextInjector,
};
use futures_util::StreamExt;
use std::{env, process::Command, sync::mpsc as std_mpsc, thread, time::Duration};
use tokio::sync::mpsc;

use crate::{linux_clipboard::LinuxClipboard, linux_remote_desktop::RemoteDesktopSessionManager};

const HOTKEY_ID: &str = "codex-voice-hold-to-dictate";
const MEDIA_HOTKEY_ID: &str = "codex-voice-media-dictation";
const HOTKEY_TRIGGER: &str = "<Control>m";
const MEDIA_HOTKEY_TRIGGER: &str = "<Super>h";
const HOTKEY_DESCRIPTION: &str = "Hold to dictate with Codex Voice";
const MEDIA_HOTKEY_DESCRIPTION: &str =
    "Hold the keyboard dictation key to dictate with Codex Voice";
const HOTKEY_START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Default, Clone)]
pub struct LinuxPermissionService;

impl LinuxPermissionService {
    pub fn new() -> Self {
        Self
    }

    pub fn portal_report(&self) -> Vec<PermissionStatus> {
        vec![
            portal_status(
                PermissionKind::GlobalShortcut,
                "org.freedesktop.portal.GlobalShortcuts",
            ),
            portal_status(
                PermissionKind::RemoteDesktopKeyboard,
                "org.freedesktop.portal.RemoteDesktop",
            ),
            PermissionStatus {
                kind: PermissionKind::Microphone,
                available: true,
                granted: None,
                detail: "microphone permission is verified by opening the CPAL input stream".into(),
            },
        ]
    }
}

#[async_trait]
impl PermissionService for LinuxPermissionService {
    async fn check(&self) -> PlatformResult<Vec<PermissionStatus>> {
        Ok(self.portal_report())
    }

    async fn request_or_open_settings(&self, permission: PermissionKind) -> PlatformResult<()> {
        Err(PlatformError::Unavailable(format!(
            "{permission:?} permission must be granted through the KDE/xdg-desktop-portal prompt when the operation starts"
        )))
    }
}

#[derive(Debug, Clone)]
pub struct LinuxTextInjector {
    remote_desktop: RemoteDesktopSessionManager,
    restore_clipboard_after: Duration,
}

impl Default for LinuxTextInjector {
    fn default() -> Self {
        Self {
            remote_desktop: RemoteDesktopSessionManager::new(),
            restore_clipboard_after: Duration::from_millis(250),
        }
    }
}

impl LinuxTextInjector {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TextInjector for LinuxTextInjector {
    async fn insert_text(&self, text: &str) -> PlatformResult<InsertReport> {
        let clipboard = LinuxClipboard::new()?;
        let previous = clipboard.snapshot();
        clipboard.set_text(text)?;

        let paste_result = self.remote_desktop.send_paste_chord().await;
        tokio::time::sleep(self.restore_clipboard_after).await;

        let restored_clipboard = clipboard.restore(previous);
        paste_result?;
        Ok(InsertReport {
            method: InsertMethod::PortalPaste,
            restored_clipboard,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct LinuxHotkeyService;

impl LinuxHotkeyService {
    pub fn new() -> Self {
        Self
    }
}

impl HotkeyService for LinuxHotkeyService {
    fn start(&self, events: mpsc::Sender<HotkeyEvent>) -> PlatformResult<()> {
        if env::var("XDG_SESSION_TYPE").unwrap_or_default() != "wayland" {
            return Err(PlatformError::Unavailable(
                "Linux hotkey service currently targets KDE/Wayland only".into(),
            ));
        }

        let (startup_tx, startup_rx) = std_mpsc::channel();
        thread::spawn(move || {
            let startup_for_error = startup_tx.clone();
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = startup_tx.send(Err(PlatformError::Unavailable(format!(
                        "failed to start hotkey portal runtime: {error}"
                    ))));
                    tracing::error!(message = %error, "failed to start hotkey portal runtime");
                    return;
                }
            };
            if let Err(error) = runtime.block_on(run_global_shortcut_listener(events, startup_tx)) {
                let message = error.to_string();
                let _ = startup_for_error.send(Err(error));
                tracing::error!(message = %message, "GlobalShortcuts portal listener stopped");
            }
        });

        startup_rx
            .recv_timeout(HOTKEY_START_TIMEOUT)
            .map_err(|error| match error {
                std_mpsc::RecvTimeoutError::Timeout => PlatformError::PermissionDenied(
                    "timed out waiting for the GlobalShortcuts portal approval prompt".into(),
                ),
                std_mpsc::RecvTimeoutError::Disconnected => PlatformError::Unavailable(
                    "GlobalShortcuts portal listener stopped before startup completed".into(),
                ),
            })?
    }
}

fn portal_status(kind: PermissionKind, interface: &str) -> PermissionStatus {
    match portal_version(interface) {
        Ok(version) => PermissionStatus {
            kind,
            available: true,
            granted: None,
            detail: format!("{interface} available, version {version}"),
        },
        Err(error) => PermissionStatus {
            kind,
            available: false,
            granted: None,
            detail: error,
        },
    }
}

fn portal_version(interface: &str) -> Result<String, String> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            interface,
            "version",
        ])
        .output()
        .map_err(|error| format!("failed to run busctl for {interface}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "{interface} unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

async fn run_global_shortcut_listener(
    events: mpsc::Sender<HotkeyEvent>,
    startup: std_mpsc::Sender<PlatformResult<()>>,
) -> PlatformResult<()> {
    let portal = GlobalShortcuts::new().await.map_err(|error| {
        PlatformError::Unavailable(format!(
            "failed to create GlobalShortcuts portal proxy: {error}"
        ))
    })?;
    let session = portal
        .create_session(Default::default())
        .await
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "failed to create GlobalShortcuts portal session: {error}"
            ))
        })?;
    let shortcuts = [
        NewShortcut::new(HOTKEY_ID, HOTKEY_DESCRIPTION).preferred_trigger(Some(HOTKEY_TRIGGER)),
        NewShortcut::new(MEDIA_HOTKEY_ID, MEDIA_HOTKEY_DESCRIPTION)
            .preferred_trigger(Some(MEDIA_HOTKEY_TRIGGER)),
    ];
    portal
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "failed to request GlobalShortcuts binding: {error}"
            ))
        })?
        .response()
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "GlobalShortcuts binding was not approved: {error}"
            ))
        })?;

    tracing::info!(
        shortcut_id = HOTKEY_ID,
        preferred_trigger = HOTKEY_TRIGGER,
        media_shortcut_id = MEDIA_HOTKEY_ID,
        media_preferred_trigger = MEDIA_HOTKEY_TRIGGER,
        "GlobalShortcuts portal listener started"
    );

    let mut activated = portal.receive_activated().await.map_err(|error| {
        PlatformError::Unavailable(format!(
            "failed to subscribe to GlobalShortcuts Activated signals: {error}"
        ))
    })?;
    let mut deactivated = portal.receive_deactivated().await.map_err(|error| {
        PlatformError::Unavailable(format!(
            "failed to subscribe to GlobalShortcuts Deactivated signals: {error}"
        ))
    })?;
    let _ = startup.send(Ok(()));

    loop {
        tokio::select! {
            event = activated.next() => match event {
                Some(event) if is_dictation_shortcut(event.shortcut_id()) => {
                    if events.send(HotkeyEvent::Pressed).await.is_err() {
                        break;
                    }
                }
                Some(_) => {}
                None => break,
            },
            event = deactivated.next() => match event {
                Some(event) if is_dictation_shortcut(event.shortcut_id()) => {
                    if events.send(HotkeyEvent::Released).await.is_err() {
                        break;
                    }
                }
                Some(_) => {}
                None => break,
            },
        }
    }

    Ok(())
}

fn is_dictation_shortcut(shortcut_id: &str) -> bool {
    matches!(shortcut_id, HOTKEY_ID | MEDIA_HOTKEY_ID)
}
