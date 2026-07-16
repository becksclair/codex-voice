use async_trait::async_trait;
use codex_voice_core::{
    HotkeyEvent, HotkeyService, InsertMethod, InsertReport, PermissionKind, PermissionService,
    PermissionStatus, PlatformError, PlatformResult, SelectedText, SelectedTextReader,
    TextInjector,
};
use std::{
    ffi::{c_char, c_void},
    thread,
    time::Duration,
};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// CF release guard
// ---------------------------------------------------------------------------

struct ReleaseOnDrop(*mut c_void);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct MacOSPermissionService;

impl MacOSPermissionService {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PermissionService for MacOSPermissionService {
    async fn check(&self) -> PlatformResult<Vec<PermissionStatus>> {
        Ok(vec![
            PermissionStatus {
                kind: PermissionKind::Microphone,
                available: true,
                granted: None,
                detail: "microphone permission is verified by opening the CPAL input stream; grant via System Settings > Privacy & Security > Microphone".into(),
            },
            PermissionStatus {
                kind: PermissionKind::Accessibility,
                available: true,
                granted: Some(is_accessibility_trusted()),
                detail: "Accessibility permission is required for text insertion; grant via System Settings > Privacy & Security > Accessibility".into(),
            },
            PermissionStatus {
                kind: PermissionKind::GlobalShortcut,
                available: true,
                granted: None,
                detail: "Global shortcuts use global-hotkey crate (Carbon/CGEvent)".into(),
            },
        ])
    }

    async fn request_or_open_settings(&self, permission: PermissionKind) -> PlatformResult<()> {
        match permission {
            PermissionKind::Accessibility => {
                // AXIsProcessTrustedWithOptions with prompt will show the system dialog
                open_accessibility_settings();
                Ok(())
            }
            PermissionKind::Microphone => {
                open_microphone_settings();
                Ok(())
            }
            _ => Err(PlatformError::Unavailable(format!(
                "{permission:?} does not have a settings flow on macOS yet"
            ))),
        }
    }
}

fn is_accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrustedWithOptions(std::ptr::null_mut()) }
}

fn open_accessibility_settings() {
    if let Ok(mut child) = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn open_microphone_settings() {
    if let Ok(mut child) = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

// ---------------------------------------------------------------------------
// Text Injection
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct MacOSTextInjector;

impl MacOSTextInjector {
    pub fn new() -> Self {
        Self
    }

    async fn insert_via_accessibility(&self, text: &str) -> PlatformResult<InsertReport> {
        if !is_accessibility_trusted() {
            return Err(PlatformError::Unavailable(
                "Accessibility permission not granted".into(),
            ));
        }

        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if system_wide.is_null() {
            return Err(PlatformError::Unavailable(
                "failed to create system-wide AX element".into(),
            ));
        }
        let _system_wide_guard = ReleaseOnDrop(system_wide);

        let focused = get_focused_element(system_wide)?;
        if focused.is_null() {
            return Err(PlatformError::Unavailable(
                "no focused UI element found".into(),
            ));
        }
        let _focused_guard = ReleaseOnDrop(focused);

        // Try to set AXSelectedText — this replaces the selection (or inserts at cursor)
        let attr = cfstring_from_static("AXSelectedText");
        if attr.is_null() {
            return Err(PlatformError::Unavailable(
                "failed to allocate attribute CFString".into(),
            ));
        }
        let _attr_guard = ReleaseOnDrop(attr);

        let value = cfstring_from_str(text).map_err(|error| {
            PlatformError::Unavailable(format!("failed to convert text to CFString: {error}"))
        })?;
        let _value_guard = ReleaseOnDrop(value);

        let result = unsafe { AXUIElementSetAttributeValue(focused, attr, value) };

        if result == 0 {
            // kAXErrorSuccess
            return Ok(InsertReport {
                method: InsertMethod::Accessibility,
                restored_clipboard: false,
            });
        }

        Err(PlatformError::Message(format!(
            "AX set selected text failed with error {result}"
        )))
    }

    async fn insert_via_clipboard(&self, text: &str) -> PlatformResult<InsertReport> {
        let mut clipboard = arboard::Clipboard::new().map_err(|error| {
            PlatformError::Unavailable(format!("failed to open clipboard: {error}"))
        })?;
        let previous = clipboard.get_text().ok();
        clipboard
            .set_text(text.to_owned())
            .map_err(|error| PlatformError::Message(format!("failed to set clipboard: {error}")))?;

        tokio::time::sleep(Duration::from_millis(50)).await;
        send_cmd_v();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let restored_clipboard = restore_clipboard(&mut clipboard, previous);
        Ok(InsertReport {
            method: InsertMethod::ClipboardPaste,
            restored_clipboard,
        })
    }
}

#[async_trait]
impl TextInjector for MacOSTextInjector {
    async fn insert_text(&self, text: &str) -> PlatformResult<InsertReport> {
        // Try Accessibility API first, then fall back to clipboard + CGEvent paste.
        match self.insert_via_accessibility(text).await {
            Ok(report) => Ok(report),
            Err(error) => {
                tracing::warn!(%error, "accessibility insertion failed, falling back to clipboard paste");
                self.insert_via_clipboard(text).await
            }
        }
    }
}

#[async_trait]
impl SelectedTextReader for MacOSTextInjector {
    async fn selected_text(&self) -> PlatformResult<SelectedText> {
        match self.selected_text_via_accessibility().await {
            Ok(text) => return Ok(text),
            Err(error) => {
                tracing::warn!(%error, "accessibility selected-text read failed, falling back to clipboard copy");
            }
        }
        self.selected_text_via_clipboard().await
    }
}

impl MacOSTextInjector {
    async fn selected_text_via_accessibility(&self) -> PlatformResult<SelectedText> {
        if !is_accessibility_trusted() {
            return Err(PlatformError::Unavailable(
                "Accessibility permission not granted".into(),
            ));
        }

        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if system_wide.is_null() {
            return Err(PlatformError::Unavailable(
                "failed to create system-wide AX element".into(),
            ));
        }
        let _system_wide_guard = ReleaseOnDrop(system_wide);

        let focused = get_focused_element(system_wide)?;
        let _focused_guard = ReleaseOnDrop(focused);
        let attr = cfstring_from_static("AXSelectedText");
        if attr.is_null() {
            return Err(PlatformError::Unavailable(
                "failed to allocate attribute CFString".into(),
            ));
        }
        let _attr_guard = ReleaseOnDrop(attr);

        let mut value: *mut c_void = std::ptr::null_mut();
        let result = unsafe { AXUIElementCopyAttributeValue(focused, attr, &mut value) };
        if result != 0 || value.is_null() {
            return Err(PlatformError::Unavailable(format!(
                "AX selected text unavailable with error {result}"
            )));
        }
        let _value_guard = ReleaseOnDrop(value);
        let text = cfstring_to_string(value)
            .ok_or_else(|| PlatformError::Unavailable("selected text is not UTF-8".into()))?;
        if text.is_empty() {
            return Err(PlatformError::Unavailable("no selected text found".into()));
        }
        Ok(SelectedText {
            chars: text.chars().count(),
            text,
            restored_clipboard: true,
        })
    }

    async fn selected_text_via_clipboard(&self) -> PlatformResult<SelectedText> {
        let mut clipboard = arboard::Clipboard::new().map_err(|error| {
            PlatformError::Unavailable(format!("failed to open clipboard: {error}"))
        })?;
        let previous = clipboard.get_text().ok();
        let sentinel = selection_sentinel();
        clipboard
            .set_text(sentinel.clone())
            .map_err(|error| PlatformError::Message(format!("failed to set clipboard: {error}")))?;

        tokio::time::sleep(Duration::from_millis(50)).await;
        send_cmd_c();
        tokio::time::sleep(Duration::from_millis(120)).await;

        let copied = clipboard.get_text().ok();
        let restored_clipboard = restore_clipboard(&mut clipboard, previous);
        match copied {
            Some(text) if !text.is_empty() && text != sentinel => Ok(SelectedText {
                chars: text.chars().count(),
                text,
                restored_clipboard,
            }),
            _ => Err(PlatformError::Unavailable("no selected text found".into())),
        }
    }
}

fn get_focused_element(system_wide: AXUIElementRef) -> PlatformResult<AXUIElementRef> {
    let attr = cfstring_from_static("AXFocusedUIElement");
    if attr.is_null() {
        return Err(PlatformError::Unavailable(
            "failed to allocate attribute CFString".into(),
        ));
    }
    let _attr_guard = ReleaseOnDrop(attr);

    let mut focused: AXUIElementRef = std::ptr::null_mut();
    let result = unsafe {
        AXUIElementCopyAttributeValue(system_wide, attr, &mut focused as *mut AXUIElementRef)
    };

    if result == 0 && !focused.is_null() {
        Ok(focused)
    } else {
        Err(PlatformError::Unavailable(
            "no focused UI element (Accessibility permission may be denied)".into(),
        ))
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

// ---------------------------------------------------------------------------
// Hotkey
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MacOSHotkeyService {
    manager: std::sync::Arc<std::sync::Mutex<global_hotkey::GlobalHotKeyManager>>,
}

impl MacOSHotkeyService {
    pub fn new() -> PlatformResult<Self> {
        let manager = global_hotkey::GlobalHotKeyManager::new().map_err(|error| {
            PlatformError::Unavailable(format!("failed to create global hotkey manager: {error}"))
        })?;
        Ok(Self {
            manager: std::sync::Arc::new(std::sync::Mutex::new(manager)),
        })
    }
}

impl HotkeyService for MacOSHotkeyService {
    fn start(&self, events: mpsc::Sender<HotkeyEvent>) -> PlatformResult<()> {
        use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, HotKeyState};

        let dictation_hotkey = HotKey::new(
            Some(global_hotkey::hotkey::Modifiers::CONTROL),
            global_hotkey::hotkey::Code::KeyM,
        );
        let speak_hotkey = HotKey::new(
            Some(global_hotkey::hotkey::Modifiers::SUPER),
            global_hotkey::hotkey::Code::F6,
        );

        self.manager
            .lock()
            .map_err(|_| PlatformError::Unavailable("hotkey manager lock poisoned".into()))?
            .register_all(&[dictation_hotkey, speak_hotkey])
            .map_err(|error| {
                PlatformError::Unavailable(format!("failed to register global hotkey: {error}"))
            })?;

        thread::Builder::new()
            .name("codex-voice-macos-hotkey".into())
            .spawn(move || {
                let receiver = GlobalHotKeyEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.id == dictation_hotkey.id() {
                        let hotkey_event = match event.state {
                            HotKeyState::Pressed => HotkeyEvent::Pressed,
                            HotKeyState::Released => HotkeyEvent::Released,
                        };
                        if events.blocking_send(hotkey_event).is_err() {
                            break;
                        }
                    } else if event.id == speak_hotkey.id()
                        && event.state == HotKeyState::Pressed
                        && events.blocking_send(HotkeyEvent::SpeakSelection).is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| {
                PlatformError::Unavailable(format!("failed to start macOS hotkey thread: {error}"))
            })?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CGEvent paste fallback (Command+V)
// ---------------------------------------------------------------------------

fn send_cmd_c() {
    send_cmd_chord(K_VK_ANSI_C);
}

fn send_cmd_v() {
    send_cmd_chord(K_VK_ANSI_V);
}

fn send_cmd_chord(key: u16) {
    let source = unsafe { CGEventSourceCreate(K_CGEVENT_SOURCE_STATE_HID_SYSTEM_STATE) };
    if source.is_null() {
        tracing::warn!("CGEventSourceCreate returned null; keyboard chord will not work");
        return;
    }
    let _source_guard = ReleaseOnDrop(source);

    let cmd_down = unsafe { CGEventCreateKeyboardEvent(source, K_VK_COMMAND, true) };
    let key_down = unsafe { CGEventCreateKeyboardEvent(source, key, true) };
    let key_up = unsafe { CGEventCreateKeyboardEvent(source, key, false) };
    let cmd_up = unsafe { CGEventCreateKeyboardEvent(source, K_VK_COMMAND, false) };

    let _cmd_down_guard = ReleaseOnDrop(cmd_down);
    let _key_down_guard = ReleaseOnDrop(key_down);
    let _key_up_guard = ReleaseOnDrop(key_up);
    let _cmd_up_guard = ReleaseOnDrop(cmd_up);

    if cmd_down.is_null() || key_down.is_null() || key_up.is_null() || cmd_up.is_null() {
        tracing::warn!("CGEventCreateKeyboardEvent returned null; keyboard chord will not work");
        return;
    }

    unsafe {
        CGEventSetFlags(cmd_down, K_CGEVENT_FLAG_MASK_COMMAND);
        CGEventSetFlags(key_down, K_CGEVENT_FLAG_MASK_COMMAND);
        CGEventSetFlags(key_up, K_CGEVENT_FLAG_MASK_COMMAND);
        CGEventSetFlags(cmd_up, K_CGEVENT_FLAG_MASK_COMMAND);

        CGEventPost(K_CGHID_EVENT_TAP, cmd_down);
        CGEventPost(K_CGHID_EVENT_TAP, key_down);
        CGEventPost(K_CGHID_EVENT_TAP, key_up);
        CGEventPost(K_CGHID_EVENT_TAP, cmd_up);
    }
}

// ---------------------------------------------------------------------------
// Accessibility / CoreFoundation FFI
// ---------------------------------------------------------------------------

type AXUIElementRef = *mut c_void;
type CFStringRef = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *mut c_void) -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *mut c_void,
        cstr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFStringGetLength(the_string: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFRelease(cf: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(stateID: i32) -> *mut c_void;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, keydown: bool) -> *mut c_void;
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
}

const K_CFSTRING_ENCODING_UTF8: u32 = 0x08000100;
const K_CGEVENT_SOURCE_STATE_HID_SYSTEM_STATE: i32 = 1;
const K_CGHID_EVENT_TAP: u32 = 0;
const K_CGEVENT_FLAG_MASK_COMMAND: u64 = 0x00100000;
const K_VK_COMMAND: u16 = 0x37;
const K_VK_ANSI_C: u16 = 0x08;
const K_VK_ANSI_V: u16 = 0x09;

fn cfstring_from_static(s: &'static str) -> CFStringRef {
    let cstr = std::ffi::CString::new(s)
        .expect("static string literals must not contain interior nul bytes");
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null_mut(),
            cstr.as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        )
    }
}

fn cfstring_from_str(s: &str) -> PlatformResult<CFStringRef> {
    let cstr = std::ffi::CString::new(s)
        .map_err(|_| PlatformError::Message("text contains interior nul byte".into()))?;
    let ptr = unsafe {
        CFStringCreateWithCString(
            std::ptr::null_mut(),
            cstr.as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        )
    };
    if ptr.is_null() {
        return Err(PlatformError::Unavailable(
            "failed to allocate CFString".into(),
        ));
    }
    Ok(ptr)
}

fn cfstring_to_string(value: *mut c_void) -> Option<String> {
    let cf = value as CFStringRef;
    let length = unsafe { CFStringGetLength(cf) };
    if length < 0 {
        return None;
    }
    let max = unsafe { CFStringGetMaximumSizeForEncoding(length, K_CFSTRING_ENCODING_UTF8) };
    if max < 0 {
        return None;
    }
    let mut buffer = vec![0_u8; max as usize + 1];
    let ok = unsafe {
        CFStringGetCString(
            cf,
            buffer.as_mut_ptr() as *mut c_char,
            buffer.len() as isize,
            K_CFSTRING_ENCODING_UTF8,
        )
    };
    if !ok {
        return None;
    }
    let nul = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8(buffer[..nul].to_vec()).ok()
}

fn selection_sentinel() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "codex-voice-selection-sentinel-{}-{}",
        std::process::id(),
        nanos
    )
}
