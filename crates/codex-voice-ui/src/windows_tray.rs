use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
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
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{GetStockObject, UpdateWindow, WHITE_BRUSH},
    System::LibraryLoader::GetModuleHandleW,
    System::SystemServices::SS_LEFT,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, IsWindow, PeekMessageW,
        RegisterClassExW, SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage,
        UnregisterClassW, CW_USEDEFAULT, MSG, PM_REMOVE, SW_SHOW, WM_CLOSE, WM_DESTROY,
        WNDCLASSEXW, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    },
};

use crate::tray_common::{
    build_icon_cache, icon_for_state, UiCommand, MENU_DIAGNOSTICS, MENU_LOGS, MENU_QUIT,
    MENU_SETTINGS, MENU_SPEAK_TEXT, MENU_STATUS, MENU_TEST_RECORDING,
};
use crate::UiStatus;

const SETTINGS_CLASS_NAME: &str = "CodexVoiceSettingsWindow";

#[derive(Debug, Clone)]
pub struct WindowsUiConfig {
    pub log_path: PathBuf,
}

pub struct StatusTray {
    status_tx: Sender<UiStatus>,
    command_rx: Receiver<UiCommand>,
    _thread: thread::JoinHandle<()>,
}

impl StatusTray {
    pub fn start(initial: UiStatus, config: WindowsUiConfig) -> Result<Self, String> {
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

    pub fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus> {
        self.status_tx.clone()
    }
}

fn run_tray(
    initial: UiStatus,
    config: WindowsUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), String>>,
) {
    let result = initialize_tray(initial, config, status_rx, command_tx, ready_tx.clone());
    if let Err(error) = result {
        let _ = ready_tx.send(Err(error.clone()));
        eprintln!("codex-voice tray stopped: {error}");
    }
}

fn initialize_tray(
    initial: UiStatus,
    config: WindowsUiConfig,
    status_rx: Receiver<UiStatus>,
    command_tx: Sender<UiCommand>,
    ready_tx: Sender<Result<(), String>>,
) -> Result<(), String> {
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
    .map_err(|error| format!("failed to build tray menu: {error}"))?;

    let icons = build_icon_cache().map_err(|e| format!("failed to build icon cache: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon_for_state(&icons, &initial.state))
        .with_title(initial.title())
        .with_tooltip("Codex Voice")
        .build()
        .map_err(|error| format!("failed to create tray icon: {error}"))?;

    let _ = ready_tx.send(Ok(()));
    let mut current_status = initial;

    loop {
        while let Ok(status) = status_rx.try_recv() {
            current_status = status;
            status_item.set_text(current_status.tray_label());
            tray.set_title(Some(current_status.title()));
            tray.set_icon(Some(icon_for_state(&icons, &current_status.state)))
                .map_err(|error| format!("failed to update tray icon: {error}"))?;
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
                    show_settings_window(&config, &current_status);
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

fn show_settings_window(config: &WindowsUiConfig, initial: &UiStatus) {
    let config = config.clone();
    let initial = initial.clone();
    thread::spawn(move || {
        let _ = run_settings_window(config, initial);
    });
}

fn show_speak_text_dialog(command_tx: Sender<UiCommand>) {
    thread::spawn(move || {
        let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$form = New-Object System.Windows.Forms.Form
$form.Text = 'Speak Text'
$form.Width = 620
$form.Height = 430
$text = New-Object System.Windows.Forms.TextBox
$text.Multiline = $true
$text.ScrollBars = 'Vertical'
$text.AcceptsReturn = $true
$text.AcceptsTab = $true
$text.SetBounds(12,12,580,300)
$generate = New-Object System.Windows.Forms.Button
$generate.Text = 'Generate'
$generate.SetBounds(12,325,95,32)
$play = New-Object System.Windows.Forms.Button
$play.Text = 'Play'
$play.SetBounds(116,325,75,32)
$close = New-Object System.Windows.Forms.Button
$close.Text = 'Close'
$close.SetBounds(500,325,75,32)
$generate.Add_Click({ [Console]::Out.WriteLine('GENERATE:' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text.Text))); $form.Close() })
$play.Add_Click({ [Console]::Out.WriteLine('PLAY'); $form.Close() })
$close.Add_Click({ $form.Close() })
$form.Controls.AddRange(@($text,$generate,$play,$close))
[void]$form.ShowDialog()
"#;
        let output = Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output();
        if let Ok(output) = output {
            let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if line == "PLAY" {
                let _ = command_tx.send(UiCommand::PlayLastSpeech);
            } else if let Some(encoded) = line.strip_prefix("GENERATE:") {
                if let Ok(bytes) = decode_base64(encoded) {
                    if let Ok(text) = String::from_utf8(bytes) {
                        let _ = command_tx.send(UiCommand::SpeakText(text));
                    }
                }
            }
        }
    });
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ()> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let Some(value) = TABLE.iter().position(|candidate| *candidate == byte) else {
            return Err(());
        };
        buf = (buf << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn run_settings_window(config: WindowsUiConfig, initial: UiStatus) -> Result<(), String> {
    let class_name = to_wide(SETTINGS_CLASS_NAME);
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };

    static REGISTER_ONCE: std::sync::Once = std::sync::Once::new();
    REGISTER_ONCE.call_once(|| {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(settings_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: unsafe { GetStockObject(WHITE_BRUSH) },
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        unsafe { RegisterClassExW(&wc) };
    });

    let title = to_wide("Codex Voice Settings");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW & !0x00040000, // WS_THICKFRAME removed
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            480,
            320,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        )
    };

    if hwnd == std::ptr::null_mut() {
        unsafe { UnregisterClassW(class_name.as_ptr(), hinstance) };
        return Err("failed to create settings window".to_string());
    }

    // Create a multi-line static text control as a child window
    let static_class = to_wide("STATIC");
    let text_hwnd = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            static_class.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            12,
            12,
            440,
            260,
            hwnd,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null_mut(),
        )
    };

    let initial_text = build_settings_text(&config, &initial);
    let wide_text = to_wide(&initial_text);
    unsafe { SetWindowTextW(text_hwnd, wide_text.as_ptr()) };

    unsafe { ShowWindow(hwnd, SW_SHOW) };
    unsafe { UpdateWindow(hwnd) };
    unsafe { SetForegroundWindow(hwnd) };

    let mut msg = MSG::default();
    loop {
        // Process window messages
        while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if msg.message == 0x0012 {
                // WM_QUIT
                break;
            }
            unsafe { TranslateMessage(&msg) };
            unsafe { DispatchMessageW(&msg) };
        }

        // If window was destroyed, exit
        if unsafe { IsWindow(hwnd) } == 0 {
            break;
        }

        thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

extern "system" fn settings_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // Don't PostQuitMessage here — the outer loop checks IsWindow
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn build_settings_text(config: &WindowsUiConfig, status: &UiStatus) -> String {
    format!(
        "Status: {}\r\n\r\n\
         Hotkeys: Control-M (GetAsyncKeyState polling)\r\n\
         Insertion: Clipboard + SendInput (Ctrl+V)\r\n\
         Transcription: Codex auth from ~/.codex/auth.json\r\n\
         Timeout: default runtime timeout\r\n\
         Debug logs: set RUST_LOG before launching\r\n\r\n\
         Log file: {}",
        status.message,
        config.log_path.display()
    )
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
