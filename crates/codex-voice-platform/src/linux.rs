use ashpd::desktop::global_shortcuts::{
    BindShortcutsOptions, GlobalShortcuts, ListShortcutsOptions, NewShortcut,
};
use async_trait::async_trait;
use codex_voice_core::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, SelectedText, SelectedTextReader,
    TextInjector,
};
use futures_util::StreamExt;
use std::{env, process::Command, sync::mpsc as std_mpsc, thread, time::Duration};
use tokio::sync::mpsc;

use crate::{
    linux_clipboard::LinuxClipboard,
    linux_portal_identity::{register_portal_app, PORTAL_APP_ID},
    linux_remote_desktop::RemoteDesktopSessionManager,
};

const HOTKEY_ID: &str = "codex-voice-hold-to-dictate";
const MEDIA_HOTKEY_ID: &str = "codex-voice-media-dictation";
const SPEAK_SELECTION_HOTKEY_ID: &str = "codex-voice-speak-selection";
const HOTKEY_TRIGGER: &str = "<Control>m";
const MEDIA_HOTKEY_TRIGGER: &str = "<Super>h";
const SPEAK_SELECTION_TRIGGER: &str = "<Super>F6";
const HOTKEY_DESCRIPTION: &str = "Hold to dictate with Codex Voice";
const MEDIA_HOTKEY_DESCRIPTION: &str =
    "Hold the keyboard dictation key to dictate with Codex Voice";
const SPEAK_SELECTION_DESCRIPTION: &str = "Speak selected text with Codex Voice";
const HOTKEY_START_TIMEOUT: Duration = Duration::from_secs(15);
const HOTKEY_RELEASE_SETTLE: Duration = Duration::from_millis(80);
const COPY_SETTLE: Duration = Duration::from_millis(120);
const EXPECTED_SHORTCUT_IDS: [&str; 3] = [HOTKEY_ID, MEDIA_HOTKEY_ID, SPEAK_SELECTION_HOTKEY_ID];

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

#[async_trait]
impl SelectedTextReader for LinuxTextInjector {
    async fn selected_text(&self) -> PlatformResult<SelectedText> {
        let clipboard = LinuxClipboard::new()?;
        let previous = clipboard.snapshot();
        let sentinel = selection_sentinel();
        clipboard.set_text(&sentinel)?;

        let copy_result = self.remote_desktop.send_copy_chord().await;
        tokio::time::sleep(COPY_SETTLE).await;
        let copied = clipboard.snapshot();
        let restored_clipboard = clipboard.restore(previous);
        copy_result?;

        match selected_text_from_snapshot(copied, &sentinel, restored_clipboard)
            .or_else(|| selected_text_from_snapshot(clipboard.primary_selection(), "", true))
        {
            Some(selection) => Ok(selection),
            _ => Err(PlatformError::Unavailable("no selected text found".into())),
        }
    }
}

fn selected_text_from_snapshot(
    snapshot: crate::linux_clipboard::ClipboardSnapshot,
    sentinel: &str,
    restored_clipboard: bool,
) -> Option<SelectedText> {
    match snapshot {
        crate::linux_clipboard::ClipboardSnapshot::Text(text)
            if !text.is_empty() && text != sentinel =>
        {
            Some(SelectedText {
                chars: text.chars().count(),
                text,
                restored_clipboard,
            })
        }
        _ => None,
    }
}

fn selection_sentinel() -> String {
    format!(
        "codex-voice-selection-sentinel-{}-{}",
        std::process::id(),
        rand_suffix()
    )
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Default, Clone)]
pub struct LinuxHotkeyService;

impl LinuxHotkeyService {
    pub fn new() -> Self {
        Self
    }
}

impl HotkeyService for LinuxHotkeyService {
    /// Spawns an OS thread and builds a dedicated `current_thread` Tokio runtime
    /// to run the GlobalShortcuts portal listener.  Callers that are already
    /// inside a Tokio runtime should expect this side effect.
    fn start(&self, events: mpsc::Sender<HotkeyEvent>) -> PlatformResult<()> {
        if !has_wayland_hotkey_session() {
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
    register_portal_app().await?;
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
        NewShortcut::new(SPEAK_SELECTION_HOTKEY_ID, SPEAK_SELECTION_DESCRIPTION)
            .preferred_trigger(Some(SPEAK_SELECTION_TRIGGER)),
    ];
    match expected_shortcuts_are_bound(&portal, &session).await {
        Ok(true) => {
            tracing::info!(
                app_id = PORTAL_APP_ID,
                "GlobalShortcuts bindings already configured; attaching current session"
            );
        }
        Ok(false) => {
            tracing::info!(
                app_id = PORTAL_APP_ID,
                "GlobalShortcuts bindings missing; requesting binding"
            );
        }
        Err(error) => {
            tracing::warn!(
                message = %error,
                app_id = PORTAL_APP_ID,
                "failed to preflight GlobalShortcuts bindings; requesting binding"
            );
        }
    }

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
        app_id = PORTAL_APP_ID,
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
                Some(event) if event.shortcut_id() == SPEAK_SELECTION_HOTKEY_ID => {
                    tokio::time::sleep(HOTKEY_RELEASE_SETTLE).await;
                    if events.send(HotkeyEvent::SpeakSelection).await.is_err() {
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

async fn expected_shortcuts_are_bound(
    portal: &GlobalShortcuts,
    session: &ashpd::desktop::Session<GlobalShortcuts>,
) -> PlatformResult<bool> {
    let response = portal
        .list_shortcuts(session, ListShortcutsOptions::default())
        .await
        .map_err(|error| {
            PlatformError::Unavailable(format!("failed to request GlobalShortcuts list: {error}"))
        })?
        .response()
        .map_err(|error| {
            PlatformError::Unavailable(format!("failed to list GlobalShortcuts bindings: {error}"))
        })?;
    let shortcuts = response.shortcuts();
    Ok(EXPECTED_SHORTCUT_IDS
        .iter()
        .all(|expected| shortcuts.iter().any(|shortcut| shortcut.id() == *expected)))
}

fn has_wayland_hotkey_session() -> bool {
    let has_wayland_session_type = env::var("XDG_SESSION_TYPE")
        .map(|session_type| session_type.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    let has_wayland_display = env::var("WAYLAND_DISPLAY")
        .map(|display| !display.trim().is_empty())
        .unwrap_or(false);

    kde_desktop_hint() && (has_wayland_session_type || has_wayland_display)
}

fn kde_desktop_hint() -> bool {
    [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|key| env::var(key).ok())
    .any(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("kde") || lower.contains("plasma")
    })
}

#[cfg(test)]
mod tests {
    use super::{has_wayland_hotkey_session, kde_desktop_hint};
    use std::env;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn with_hotkey_env(vars: &[(&str, &str)], test: impl FnOnce()) {
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock");
        let keys = [
            "XDG_SESSION_TYPE",
            "WAYLAND_DISPLAY",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_DESKTOP",
            "DESKTOP_SESSION",
        ];
        let previous: Vec<_> = keys.iter().map(|key| (*key, env::var(key).ok())).collect();
        for key in keys {
            env::remove_var(key);
        }
        for (key, value) in vars {
            env::set_var(key, value);
        }

        test();

        for key in keys {
            env::remove_var(key);
        }
        for (key, value) in previous {
            if let Some(value) = value {
                env::set_var(key, value);
            }
        }
    }

    #[test]
    fn accepts_direct_wayland_session_type() {
        with_hotkey_env(
            &[
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            || {
                assert!(has_wayland_hotkey_session());
            },
        );
    }

    #[test]
    fn rejects_direct_wayland_session_type_without_kde_hint() {
        with_hotkey_env(
            &[
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_CURRENT_DESKTOP", "sway"),
            ],
            || {
                assert!(!has_wayland_hotkey_session());
            },
        );
    }

    #[test]
    fn accepts_kde_wayland_display_without_session_type() {
        with_hotkey_env(
            &[
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            || {
                assert!(has_wayland_hotkey_session());
            },
        );
    }

    #[test]
    fn rejects_kde_hint_without_wayland() {
        with_hotkey_env(&[("XDG_CURRENT_DESKTOP", "KDE")], || {
            assert!(!has_wayland_hotkey_session());
        });
    }

    #[test]
    fn accepts_stale_tty_type_when_kde_wayland_vars_are_present() {
        with_hotkey_env(
            &[
                ("XDG_SESSION_TYPE", "tty"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE"),
            ],
            || {
                assert!(has_wayland_hotkey_session());
            },
        );
    }

    #[test]
    fn rejects_stale_tty_type_without_kde_hint() {
        with_hotkey_env(
            &[
                ("XDG_SESSION_TYPE", "tty"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "sway"),
            ],
            || {
                assert!(!has_wayland_hotkey_session());
                assert!(!kde_desktop_hint());
            },
        );
    }
}
