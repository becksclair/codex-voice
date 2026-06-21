use ashpd::{register_host_app, AppID};
use codex_voice_core::{PlatformError, PlatformResult};

pub(crate) const PORTAL_APP_ID: &str = "com.heliasar.CodexVoice";

pub(crate) async fn register_portal_app() -> PlatformResult<()> {
    let app_id = AppID::try_from(PORTAL_APP_ID).map_err(|error| {
        PlatformError::Unavailable(format!("invalid portal app id {PORTAL_APP_ID}: {error}"))
    })?;

    register_host_app(app_id).await.map_err(|error| {
        PlatformError::Unavailable(format!(
            "failed to register portal app id {PORTAL_APP_ID}: {error}"
        ))
    })
}
