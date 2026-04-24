use arboard::Clipboard;
use async_trait::async_trait;
use codex_voice_core::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, TextInjector,
};
use std::{
    env,
    io::{self, Read},
    process::Command,
    thread,
    time::Duration,
};
use tokio::sync::mpsc;

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
    restore_clipboard_after: Duration,
}

impl Default for LinuxTextInjector {
    fn default() -> Self {
        Self {
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
        let mut clipboard = Clipboard::new().map_err(|error| {
            PlatformError::Unavailable(format!("clipboard unavailable: {error}"))
        })?;
        let previous = clipboard.get_text().ok();
        clipboard
            .set_text(text.to_string())
            .map_err(|error| PlatformError::Message(format!("failed to set clipboard: {error}")))?;

        let paste_result = send_paste_chord();
        tokio::time::sleep(self.restore_clipboard_after).await;

        let restored_clipboard = match previous {
            Some(previous) => clipboard.set_text(previous).is_ok(),
            None => false,
        };
        paste_result?;
        Ok(InsertReport {
            method: InsertMethod::ClipboardPaste,
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

        thread::spawn(move || {
            tracing::warn!(
                "portal hotkey binding is not available in this diagnostic build; press Enter to simulate Control-M press/release"
            );
            let stdin = io::stdin();
            let mut handle = stdin.lock();
            let mut byte = [0_u8; 1];
            loop {
                match handle.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if byte[0] == b'\n' => {
                        let _ = events.blocking_send(HotkeyEvent::Pressed);
                        let _ = events.blocking_send(HotkeyEvent::Released);
                    }
                    Ok(_) => {}
                }
            }
        });
        Ok(())
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

fn send_paste_chord() -> PlatformResult<()> {
    if command_exists("wtype") {
        return run_paste_command("wtype", ["-M", "ctrl", "v", "-m", "ctrl"]);
    }

    if command_exists("ydotool") {
        return run_paste_command("ydotool", ["key", "29:1", "47:1", "47:0", "29:0"]);
    }

    Err(PlatformError::Unavailable(
        "no supported paste injector found; install wtype for Wayland paste diagnostics or wire the RemoteDesktop portal keyboard session".into(),
    ))
}

fn run_paste_command<const N: usize>(program: &str, args: [&str; N]) -> PlatformResult<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|error| PlatformError::Message(format!("failed to run {program}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Message(format!(
            "{program} failed with status {status}"
        )))
    }
}

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name} >/dev/null 2>&1")])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
