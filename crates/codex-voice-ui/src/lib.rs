use codex_voice_core::DictationState;

#[derive(Debug, Clone)]
pub struct UiStatus {
    pub state: DictationState,
    pub message: String,
}

impl UiStatus {
    pub fn new(state: DictationState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }
}
