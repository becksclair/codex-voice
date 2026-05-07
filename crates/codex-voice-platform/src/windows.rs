use async_trait::async_trait;
use codex_voice_core::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, TextInjector,
};
use std::{
    io,
    mem::size_of,
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VK_CONTROL, VK_LCONTROL, VK_M, VK_RCONTROL, VK_V,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PASTE_SETTLE: Duration = Duration::from_millis(80);

#[derive(Debug, Default, Clone)]
pub struct WindowsPermissionService;

impl WindowsPermissionService {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PermissionService for WindowsPermissionService {
    async fn check(&self) -> PlatformResult<Vec<PermissionStatus>> {
        Ok(vec![
            PermissionStatus {
                kind: PermissionKind::Microphone,
                available: true,
                granted: None,
                detail: "microphone permission is verified by opening the CPAL input stream".into(),
            },
            PermissionStatus {
                kind: PermissionKind::GlobalShortcut,
                available: true,
                granted: None,
                detail: "Control-M is detected with GetAsyncKeyState polling".into(),
            },
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                available: false,
                granted: None,
                detail: "Windows insertion currently uses clipboard plus SendInput; UI Automation is deferred".into(),
            },
        ])
    }

    async fn request_or_open_settings(&self, permission: PermissionKind) -> PlatformResult<()> {
        Err(PlatformError::Unavailable(format!(
            "{permission:?} does not have a Codex Voice settings flow on Windows yet"
        )))
    }
}

#[derive(Debug, Default, Clone)]
pub struct WindowsTextInjector;

impl WindowsTextInjector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TextInjector for WindowsTextInjector {
    async fn insert_text(&self, text: &str) -> PlatformResult<InsertReport> {
        let mut clipboard = arboard::Clipboard::new().map_err(|error| {
            PlatformError::Unavailable(format!("failed to open clipboard: {error}"))
        })?;
        let previous = clipboard.get_text().ok();
        clipboard
            .set_text(text.to_owned())
            .map_err(|error| PlatformError::Message(format!("failed to set clipboard: {error}")))?;

        wait_for_control_release(Duration::from_secs(2)).await;
        tokio::time::sleep(PASTE_SETTLE).await;
        let paste_result = send_ctrl_v();
        tokio::time::sleep(PASTE_SETTLE).await;

        let restored_clipboard = restore_clipboard(&mut clipboard, previous);
        paste_result?;

        Ok(InsertReport {
            method: InsertMethod::SendInputPaste,
            restored_clipboard,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct WindowsHotkeyService;

impl WindowsHotkeyService {
    pub fn new() -> Self {
        Self
    }
}

impl HotkeyService for WindowsHotkeyService {
    fn start(&self, events: mpsc::Sender<HotkeyEvent>) -> PlatformResult<()> {
        thread::Builder::new()
            .name("codex-voice-windows-hotkey".into())
            .spawn(move || poll_control_m(events))
            .map_err(|error| {
                PlatformError::Unavailable(format!(
                    "failed to start Windows hotkey thread: {error}"
                ))
            })?;
        Ok(())
    }
}

fn poll_control_m(events: mpsc::Sender<HotkeyEvent>) {
    let mut active = false;
    loop {
        let pressed = key_down(VK_M) && control_down();
        if pressed != active {
            active = pressed;
            let event = if active {
                HotkeyEvent::Pressed
            } else {
                HotkeyEvent::Released
            };
            if events.blocking_send(event).is_err() {
                break;
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

async fn wait_for_control_release(timeout: Duration) {
    let start = Instant::now();
    while control_down() && start.elapsed() < timeout {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn control_down() -> bool {
    key_down(VK_CONTROL) || key_down(VK_LCONTROL) || key_down(VK_RCONTROL)
}

fn key_down(key: u16) -> bool {
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}

fn send_ctrl_v() -> PlatformResult<()> {
    let inputs = [
        keyboard_input(VK_CONTROL, 0),
        keyboard_input(VK_V, 0),
        keyboard_input(VK_V, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        Err(PlatformError::Message(format!(
            "SendInput sent {sent}/{} events; last_os_error={error}; insertion may be blocked by UIPI/elevation or a non-interactive desktop",
            inputs.len()
        )))
    }
}

fn restore_clipboard(clipboard: &mut arboard::Clipboard, previous: Option<String>) -> bool {
    match previous {
        Some(value) => clipboard.set_text(value).is_ok(),
        None => {
            let _ = clipboard.clear();
            false
        }
    }
}

fn keyboard_input(key: u16, flags: u32) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(key as u32, MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
