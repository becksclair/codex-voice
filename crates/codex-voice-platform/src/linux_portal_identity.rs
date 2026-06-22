use ashpd::{register_host_app, AppID};
use codex_voice_core::{PlatformError, PlatformResult};
use std::sync::OnceLock;

pub(crate) const PORTAL_APP_ID: &str = "com.heliasar.CodexVoice";
static PORTAL_APP_REGISTERED: OnceLock<()> = OnceLock::new();

pub(crate) async fn register_portal_app() -> PlatformResult<()> {
    if PORTAL_APP_REGISTERED.get().is_some() {
        return Ok(());
    }

    let app_id = AppID::try_from(PORTAL_APP_ID).map_err(|error| {
        PlatformError::Unavailable(format!("invalid portal app id {PORTAL_APP_ID}: {error}"))
    })?;

    match register_host_app(app_id).await {
        Ok(()) => {
            let _ = PORTAL_APP_REGISTERED.set(());
            Ok(())
        }
        Err(error) if is_already_associated_error(&error.to_string()) => {
            let _ = PORTAL_APP_REGISTERED.set(());
            Ok(())
        }
        Err(error) => Err(PlatformError::Unavailable(format!(
            "failed to register portal app id {PORTAL_APP_ID}: {error}"
        ))),
    }
}

fn is_already_associated_error(message: &str) -> bool {
    message.contains("Connection already associated with an application ID")
}

#[cfg(test)]
mod tests {
    use super::is_already_associated_error;

    #[test]
    fn recognizes_repeat_portal_registration_error() {
        assert!(is_already_associated_error(
            "ZBus Error: org.freedesktop.portal.Error.Failed: Could not register app ID: Connection already associated with an application ID"
        ));
        assert!(!is_already_associated_error(
            "ZBus Error: org.freedesktop.portal.Error.NotAllowed: An app id is required"
        ));
    }
}
