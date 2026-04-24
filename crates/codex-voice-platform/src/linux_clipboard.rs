use arboard::Clipboard;
use codex_voice_core::{PlatformError, PlatformResult};
use std::{
    env,
    io::Write,
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
pub enum ClipboardSnapshot {
    Text(String),
    Empty,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardBackend {
    WlClipboard,
    Arboard,
}

#[derive(Debug, Clone)]
pub struct LinuxClipboard {
    backend: ClipboardBackend,
}

impl LinuxClipboard {
    pub fn new() -> PlatformResult<Self> {
        if env::var("XDG_SESSION_TYPE").unwrap_or_default() == "wayland"
            && command_exists("wl-copy")
            && command_exists("wl-paste")
        {
            return Ok(Self {
                backend: ClipboardBackend::WlClipboard,
            });
        }

        Clipboard::new()
            .map(|_| Self {
                backend: ClipboardBackend::Arboard,
            })
            .map_err(|error| PlatformError::Unavailable(format!("clipboard unavailable: {error}")))
    }

    pub fn snapshot(&self) -> ClipboardSnapshot {
        match self.backend {
            ClipboardBackend::WlClipboard => read_wl_clipboard(),
            ClipboardBackend::Arboard => match Clipboard::new().and_then(|mut c| c.get_text()) {
                Ok(text) => ClipboardSnapshot::Text(text),
                Err(_) => ClipboardSnapshot::Unavailable,
            },
        }
    }

    pub fn set_text(&self, text: &str) -> PlatformResult<()> {
        match self.backend {
            ClipboardBackend::WlClipboard => write_wl_clipboard(text),
            ClipboardBackend::Arboard => Clipboard::new()
                .map_err(|error| {
                    PlatformError::Unavailable(format!("clipboard unavailable: {error}"))
                })?
                .set_text(text.to_string())
                .map_err(|error| {
                    PlatformError::Message(format!("failed to set clipboard: {error}"))
                }),
        }
    }

    pub fn restore(&self, snapshot: ClipboardSnapshot) -> bool {
        match snapshot {
            ClipboardSnapshot::Text(text) => self.set_text(&text).is_ok(),
            ClipboardSnapshot::Empty => match self.backend {
                ClipboardBackend::WlClipboard => clear_wl_clipboard().is_ok(),
                ClipboardBackend::Arboard => false,
            },
            ClipboardSnapshot::Unavailable => false,
        }
    }
}

fn read_wl_clipboard() -> ClipboardSnapshot {
    match wl_clipboard_text_state() {
        WlClipboardTextState::Text => {}
        WlClipboardTextState::Empty => return ClipboardSnapshot::Empty,
        WlClipboardTextState::Unavailable => return ClipboardSnapshot::Unavailable,
    }

    let output = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/plain;charset=utf-8"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            ClipboardSnapshot::Text(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => ClipboardSnapshot::Unavailable,
    }
}

enum WlClipboardTextState {
    Text,
    Empty,
    Unavailable,
}

fn wl_clipboard_text_state() -> WlClipboardTextState {
    let output = Command::new("wl-paste").arg("--list-types").output();
    match output {
        Ok(output) if output.status.success() => {
            let types = String::from_utf8_lossy(&output.stdout);
            if types.lines().any(is_text_mime_type) {
                WlClipboardTextState::Text
            } else {
                WlClipboardTextState::Unavailable
            }
        }
        Ok(output)
            if String::from_utf8_lossy(&output.stderr)
                .to_ascii_lowercase()
                .contains("nothing is copied") =>
        {
            WlClipboardTextState::Empty
        }
        _ => WlClipboardTextState::Unavailable,
    }
}

fn is_text_mime_type(mime_type: &str) -> bool {
    matches!(mime_type, "TEXT" | "STRING" | "UTF8_STRING") || mime_type.starts_with("text/")
}

fn write_wl_clipboard(text: &str) -> PlatformResult<()> {
    let mut child = Command::new("wl-copy")
        .args(["--type", "text/plain;charset=utf-8"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| PlatformError::Message(format!("failed to run wl-copy: {error}")))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(PlatformError::Message(
            "failed to open wl-copy stdin".into(),
        ));
    };
    stdin.write_all(text.as_bytes()).map_err(|error| {
        PlatformError::Message(format!(
            "failed to write clipboard text to wl-copy: {error}"
        ))
    })?;
    drop(stdin);

    let status = child
        .wait()
        .map_err(|error| PlatformError::Message(format!("failed to wait for wl-copy: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Message(format!(
            "wl-copy failed with status {status}"
        )))
    }
}

fn clear_wl_clipboard() -> PlatformResult<()> {
    let status = Command::new("wl-copy")
        .arg("--clear")
        .status()
        .map_err(|error| {
            PlatformError::Message(format!("failed to clear clipboard with wl-copy: {error}"))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Message(format!(
            "wl-copy --clear failed with status {status}"
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
