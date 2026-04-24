use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc;

pub type PlatformResult<T> = Result<T, PlatformError>;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("{0}")]
    Message(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

pub trait HotkeyService: Send + Sync {
    fn start(&self, events: mpsc::Sender<HotkeyEvent>) -> PlatformResult<()>;
}

#[async_trait]
pub trait TextInjector: Send + Sync {
    async fn insert_text(&self, text: &str) -> PlatformResult<InsertReport>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertReport {
    pub method: InsertMethod,
    pub restored_clipboard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMethod {
    Accessibility,
    ClipboardPaste,
    PortalPaste,
    SendInputPaste,
    UiAutomationValuePattern,
}

#[async_trait]
pub trait PermissionService: Send + Sync {
    async fn check(&self) -> PlatformResult<Vec<PermissionStatus>>;
    async fn request_or_open_settings(&self, permission: PermissionKind) -> PlatformResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionStatus {
    pub kind: PermissionKind,
    pub available: bool,
    pub granted: Option<bool>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    Microphone,
    Accessibility,
    GlobalShortcut,
    RemoteDesktopKeyboard,
}
