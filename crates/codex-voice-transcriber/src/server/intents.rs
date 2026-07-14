use super::{authorize, ApiError, ServiceState};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const DESKTOP_INTENT_TTL: Duration = Duration::from_secs(60);
pub(crate) const MAX_DESKTOP_INTENTS: usize = 8;
pub(crate) const MAX_DESKTOP_INTENT_BYTES: usize = 32 * 1024;

pub(crate) type DesktopIntentStore = Arc<Mutex<HashMap<String, DesktopIntentRecord>>>;

#[derive(Clone)]
pub(crate) struct DesktopIntentRecord {
    text: String,
    created_at: Instant,
}

#[derive(Deserialize)]
pub(crate) struct DesktopIntentCreateRequest {
    text: String,
}

#[derive(Serialize)]
pub(crate) struct DesktopIntentCreateResponse {
    id: String,
}

#[derive(Serialize)]
pub(crate) struct DesktopIntentResponse {
    text: String,
}

pub(crate) async fn create_desktop_intent(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Json(body): Json<DesktopIntentCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    authorize(&headers, &state.auth)?;
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("desktop intent text is required"));
    }
    if body.text.len() > MAX_DESKTOP_INTENT_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "desktop intent text exceeds {MAX_DESKTOP_INTENT_BYTES} UTF-8 bytes"
        )));
    }

    let mut intents = state
        .desktop_intents
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_desktop_intents(&mut intents, Instant::now());
    if intents.len() >= MAX_DESKTOP_INTENTS {
        return Err(ApiError::too_many_requests(
            "desktop intent queue is full; try again shortly",
        ));
    }
    let id = hex::encode(rand::random::<[u8; 16]>());
    intents.insert(
        id.clone(),
        DesktopIntentRecord {
            text: body.text,
            created_at: Instant::now(),
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(DesktopIntentCreateResponse { id }),
    ))
}

pub(crate) async fn consume_desktop_intent(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let record = {
        let mut intents = state
            .desktop_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_desktop_intents(&mut intents, Instant::now());
        intents
            .remove(&id)
            .ok_or_else(|| ApiError::not_found("desktop intent was not found or has expired"))?
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(DesktopIntentResponse { text: record.text }),
    ))
}

pub(crate) async fn delete_desktop_intent(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> StatusCode {
    state
        .desktop_intents
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
    StatusCode::NO_CONTENT
}

pub(crate) fn prune_desktop_intents(
    intents: &mut HashMap<String, DesktopIntentRecord>,
    now: Instant,
) {
    intents
        .retain(|_, record| now.saturating_duration_since(record.created_at) <= DESKTOP_INTENT_TTL);
}
