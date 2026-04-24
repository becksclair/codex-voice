use ashpd::desktop::{
    remote_desktop::{
        DeviceType, KeyState, NotifyKeyboardKeycodeOptions, RemoteDesktop, SelectDevicesOptions,
    },
    PersistMode,
};
use codex_voice_core::{PlatformError, PlatformResult};
use enumflags2::BitFlags;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::linux_token_store::{new_token_record, PersistedPortalToken, PortalTokenStore};

const SESSION_START_TIMEOUT: Duration = Duration::from_secs(12);
const KEYCODE_CONTROL_LEFT: i32 = 29;
const KEYCODE_V: i32 = 47;

#[derive(Debug, Clone, Default)]
pub struct RemoteDesktopSessionManager {
    inner: Arc<Mutex<Option<ActiveRemoteDesktopSession>>>,
    token_store: Option<PortalTokenStore>,
}

#[derive(Debug)]
struct ActiveRemoteDesktopSession {
    remote_desktop: RemoteDesktop,
    session: ashpd::desktop::Session<RemoteDesktop>,
}

impl RemoteDesktopSessionManager {
    pub fn new() -> Self {
        let token_store = match PortalTokenStore::new() {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(
                    message = %error,
                    "failed to initialize persisted portal token storage; RemoteDesktop approval reuse will not survive process restarts"
                );
                None
            }
        };
        Self {
            inner: Arc::new(Mutex::new(None)),
            token_store,
        }
    }

    pub async fn send_paste_chord(&self) -> PlatformResult<()> {
        self.send_keycode_state(KEYCODE_CONTROL_LEFT, KeyState::Pressed)
            .await?;
        let key_result = self.send_keycode_raw(KEYCODE_V).await;
        let release_result = self
            .send_keycode_state(KEYCODE_CONTROL_LEFT, KeyState::Released)
            .await;
        key_result.and(release_result)
    }

    async fn send_keycode_raw(&self, keycode: i32) -> PlatformResult<()> {
        self.send_keycode_state(keycode, KeyState::Pressed).await?;
        self.send_keycode_state(keycode, KeyState::Released).await
    }

    async fn send_keycode_state(&self, keycode: i32, state: KeyState) -> PlatformResult<()> {
        let mut active = self.inner.lock().await;
        if active.is_none() {
            *active = Some(start_session_with_timeout(self.token_store.as_ref()).await?);
        }
        let session = active
            .as_ref()
            .expect("portal session should exist once started");
        let result = session
            .remote_desktop
            .notify_keyboard_keycode(
                &session.session,
                keycode,
                state,
                NotifyKeyboardKeycodeOptions::default(),
            )
            .await
            .map_err(|error| {
                PlatformError::Message(format!(
                    "failed to inject keyboard keycode through RemoteDesktop portal: {error}"
                ))
            });
        if result.is_err() {
            *active = None;
        }
        result
    }
}

async fn start_session_with_timeout(
    token_store: Option<&PortalTokenStore>,
) -> PlatformResult<ActiveRemoteDesktopSession> {
    tokio::time::timeout(SESSION_START_TIMEOUT, start_session(token_store))
        .await
        .map_err(|_| {
            PlatformError::PermissionDenied(
                "timed out waiting for the RemoteDesktop portal approval prompt".into(),
            )
        })?
}

async fn start_session(
    token_store: Option<&PortalTokenStore>,
) -> PlatformResult<ActiveRemoteDesktopSession> {
    let stored_token = load_stored_token(token_store);
    match start_session_once(stored_token.as_ref(), token_store).await {
        Ok(session) => Ok(session),
        Err(error) if stored_token.is_some() => {
            tracing::warn!(
                message = %error,
                "stored RemoteDesktop restore token was rejected; retrying with a fresh portal session"
            );
            start_session_once(None, token_store).await
        }
        Err(error) => Err(error),
    }
}

async fn start_session_once(
    stored_token: Option<&PersistedPortalToken>,
    token_store: Option<&PortalTokenStore>,
) -> PlatformResult<ActiveRemoteDesktopSession> {
    let remote_desktop = RemoteDesktop::new().await.map_err(|error| {
        PlatformError::Unavailable(format!(
            "failed to create RemoteDesktop portal proxy: {error}"
        ))
    })?;

    let session = remote_desktop
        .create_session(Default::default())
        .await
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "failed to create a RemoteDesktop portal session: {error}"
            ))
        })?;

    remote_desktop
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(BitFlags::from_flag(DeviceType::Keyboard))
                .set_restore_token(stored_token.map(|record| record.restore_token.as_str()))
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "failed to request keyboard control from RemoteDesktop portal: {error}"
            ))
        })?
        .response()
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "portal keyboard selection did not complete successfully: {error}"
            ))
        })?;

    let selected = remote_desktop
        .start(&session, None, Default::default())
        .await
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "failed to start the RemoteDesktop portal session: {error}"
            ))
        })?
        .response()
        .map_err(|error| {
            PlatformError::PermissionDenied(format!(
                "the RemoteDesktop portal session did not start successfully: {error}"
            ))
        })?;

    persist_restore_token(
        token_store,
        stored_token,
        selected.restore_token(),
        remote_desktop.version(),
    );

    Ok(ActiveRemoteDesktopSession {
        remote_desktop,
        session,
    })
}

fn load_stored_token(token_store: Option<&PortalTokenStore>) -> Option<PersistedPortalToken> {
    let token_store = token_store?;
    match token_store.load() {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(
                message = %error,
                token_path = %token_store.path().display(),
                "failed to load persisted RemoteDesktop restore token; falling back to a fresh portal session"
            );
            None
        }
    }
}

fn persist_restore_token(
    token_store: Option<&PortalTokenStore>,
    previous_record: Option<&PersistedPortalToken>,
    restore_token: Option<&str>,
    remote_desktop_version: u32,
) {
    let Some(token_store) = token_store else {
        return;
    };
    let Some(restore_token) = restore_token else {
        if previous_record.is_some() {
            tracing::warn!(
                token_path = %token_store.path().display(),
                "RemoteDesktop portal session started without a replacement restore token"
            );
        }
        return;
    };

    let record = new_token_record(restore_token.to_string(), Some(remote_desktop_version));
    match token_store.save(&record) {
        Ok(()) => {
            let action = match previous_record {
                Some(previous) if previous.restore_token != record.restore_token => "rotated",
                Some(_) => "refreshed",
                None => "stored",
            };
            tracing::info!(
                token_path = %token_store.path().display(),
                "RemoteDesktop portal restore token {action}"
            );
        }
        Err(error) => {
            tracing::warn!(
                message = %error,
                token_path = %token_store.path().display(),
                "failed to persist RemoteDesktop portal restore token"
            );
        }
    }
}
