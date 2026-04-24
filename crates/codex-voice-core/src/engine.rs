use crate::{
    AudioRecorder, HotkeyEvent, InsertReport, RecordedAudio, TextInjector, TranscriptionClient,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;

const MIN_RECORDING: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationState {
    Idle,
    Recording,
    Transcribing,
    Inserting,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    StateChanged(DictationState),
    RecordingDiscarded { duration: Duration },
    RecordingDeleted { path: PathBuf },
    TranscriptReady { chars: usize },
    Inserted(InsertReport),
    Error(String),
}

pub struct DictationEngine<A, T, I>
where
    A: AudioRecorder,
    T: TranscriptionClient,
    I: TextInjector,
{
    audio: Arc<A>,
    transcription: Arc<T>,
    injector: Arc<I>,
    events: mpsc::Sender<AppEvent>,
    state: DictationState,
}

impl<A, T, I> DictationEngine<A, T, I>
where
    A: AudioRecorder,
    T: TranscriptionClient,
    I: TextInjector,
{
    pub fn new(
        audio: Arc<A>,
        transcription: Arc<T>,
        injector: Arc<I>,
        events: mpsc::Sender<AppEvent>,
    ) -> Self {
        Self {
            audio,
            transcription,
            injector,
            events,
            state: DictationState::Idle,
        }
    }

    pub fn state(&self) -> &DictationState {
        &self.state
    }

    pub async fn handle_hotkey(&mut self, event: HotkeyEvent) {
        match event {
            HotkeyEvent::Pressed if self.state == DictationState::Idle => self.start().await,
            HotkeyEvent::Released if self.state == DictationState::Recording => self.stop().await,
            _ => {}
        }
    }

    async fn start(&mut self) {
        match self.audio.start().await {
            Ok(()) => self.set_state(DictationState::Recording).await,
            Err(error) => self.fail(error.to_string()).await,
        }
    }

    async fn stop(&mut self) {
        match self.audio.stop().await {
            Ok(Some(recording)) if recording.duration >= MIN_RECORDING => {
                self.process_recording(recording).await
            }
            Ok(Some(recording)) => {
                let _ = std::fs::remove_file(&recording.path);
                let _ = self
                    .events
                    .send(AppEvent::RecordingDiscarded {
                        duration: recording.duration,
                    })
                    .await;
                self.set_state(DictationState::Idle).await;
            }
            Ok(None) => self.set_state(DictationState::Idle).await,
            Err(error) => self.fail(error.to_string()).await,
        }
    }

    async fn process_recording(&mut self, recording: RecordedAudio) {
        let path = recording.path.clone();
        self.set_state(DictationState::Transcribing).await;
        let transcript = self.transcription.transcribe(&recording).await;
        let _ = std::fs::remove_file(&path);
        let _ = self.events.send(AppEvent::RecordingDeleted { path }).await;

        match transcript {
            Ok(text) if text.trim().is_empty() => self.set_state(DictationState::Idle).await,
            Ok(text) => {
                let chars = text.chars().count();
                let _ = self.events.send(AppEvent::TranscriptReady { chars }).await;
                self.set_state(DictationState::Inserting).await;
                match self.injector.insert_text(&text).await {
                    Ok(report) => {
                        let _ = self.events.send(AppEvent::Inserted(report)).await;
                        self.set_state(DictationState::Idle).await;
                    }
                    Err(error) => self.fail(error.to_string()).await,
                }
            }
            Err(error) => self.fail(error.to_string()).await,
        }
    }

    async fn set_state(&mut self, state: DictationState) {
        self.state = state.clone();
        let _ = self.events.send(AppEvent::StateChanged(state)).await;
    }

    async fn fail(&mut self, message: String) {
        let _ = self.events.send(AppEvent::Error(message.clone())).await;
        self.set_state(DictationState::Error(message)).await;
        self.set_state(DictationState::Idle).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioError, AudioResult, InsertMethod, PlatformResult, RecordedAudio, TranscriptionResult,
    };
    use async_trait::async_trait;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tempfile::NamedTempFile;

    struct FakeAudio {
        recording: Mutex<Option<RecordedAudio>>,
        start_error: bool,
    }

    #[async_trait]
    impl AudioRecorder for FakeAudio {
        async fn start(&self) -> AudioResult<()> {
            if self.start_error {
                Err(AudioError::Message("boom".into()))
            } else {
                Ok(())
            }
        }

        async fn stop(&self) -> AudioResult<Option<RecordedAudio>> {
            Ok(self
                .recording
                .lock()
                .map_err(|_| AudioError::Message("audio lock poisoned".into()))?
                .take())
        }

        async fn cancel(&self) -> AudioResult<()> {
            Ok(())
        }
    }

    struct FakeTranscription(String);

    #[async_trait]
    impl TranscriptionClient for FakeTranscription {
        async fn transcribe(&self, _recording: &RecordedAudio) -> TranscriptionResult<String> {
            Ok(self.0.clone())
        }
    }

    struct FakeInjector;

    #[async_trait]
    impl TextInjector for FakeInjector {
        async fn insert_text(&self, _text: &str) -> PlatformResult<InsertReport> {
            Ok(InsertReport {
                method: InsertMethod::ClipboardPaste,
                restored_clipboard: true,
            })
        }
    }

    #[tokio::test]
    async fn discards_short_recordings() {
        let file = NamedTempFile::new().unwrap();
        let recording = RecordedAudio {
            path: file.path().to_path_buf(),
            content_type: "audio/wav".into(),
            filename: "short.wav".into(),
            duration: Duration::from_millis(20),
        };
        let audio = Arc::new(FakeAudio {
            recording: Mutex::new(Some(recording)),
            start_error: false,
        });
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription("ignored".into())),
            Arc::new(FakeInjector),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        engine.handle_hotkey(HotkeyEvent::Released).await;

        let mut discarded = false;
        while let Ok(event) = rx.try_recv() {
            discarded |= matches!(event, AppEvent::RecordingDiscarded { .. });
        }
        assert!(discarded);
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn returns_to_idle_after_error() {
        let audio = Arc::new(FakeAudio {
            recording: Mutex::new(None),
            start_error: true,
        });
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription("ignored".into())),
            Arc::new(FakeInjector),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;

        let mut saw_error = false;
        while let Ok(event) = rx.try_recv() {
            saw_error |= matches!(event, AppEvent::StateChanged(DictationState::Error(_)));
        }
        assert!(saw_error);
        assert_eq!(engine.state(), &DictationState::Idle);
    }
}
