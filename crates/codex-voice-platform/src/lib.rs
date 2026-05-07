#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
mod linux_clipboard;
#[cfg(target_os = "linux")]
mod linux_remote_desktop;
#[cfg(target_os = "linux")]
mod linux_token_store;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub mod unsupported {
    use codex_voice_core::{PlatformError, PlatformResult};

    pub fn unsupported_platform<T>() -> PlatformResult<T> {
        Err(PlatformError::Unavailable(
            "this build only implements Linux and Windows in the current milestone".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub use linux::{LinuxHotkeyService, LinuxPermissionService, LinuxTextInjector};
#[cfg(target_os = "windows")]
pub use windows::{WindowsHotkeyService, WindowsPermissionService, WindowsTextInjector};
