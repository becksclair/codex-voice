use crate::{
    AudioRecorder, HotkeyEvent, InsertReport, RecordedAudio, TextInjector, TranscriptionClient,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::fs as tokio_fs;
use tokio::sync::mpsc;

const MIN_RECORDING: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DictationState {
    Idle,
    Recording,
    Transcribing,
    Inserting,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorStage {
    AudioStart,
    AudioStop,
    Transcription,
    Insertion,
}

impl ErrorStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::AudioStart => "audio start failed",
            Self::AudioStop => "audio stop failed",
            Self::Transcription => "transcription failed",
            Self::Insertion => "insertion failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    StateChanged(DictationState),
    RecordingDiscarded { duration: Duration },
    RecordingDeleted { path: PathBuf },
    TranscriptReady { chars: usize },
    Inserted(InsertReport),
    Error { stage: ErrorStage, message: String },
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
            Err(error) => self.fail(ErrorStage::AudioStart, error.to_string()).await,
        }
    }

    async fn stop(&mut self) {
        match self.audio.stop().await {
            Ok(Some(recording)) if recording.duration >= MIN_RECORDING => {
                self.process_recording(recording).await
            }
            Ok(Some(recording)) => {
                let _ = tokio_fs::remove_file(&recording.path).await;
                let _ = self
                    .events
                    .send(AppEvent::RecordingDiscarded {
                        duration: recording.duration,
                    })
                    .await;
                self.set_state(DictationState::Idle).await;
            }
            Ok(None) => self.set_state(DictationState::Idle).await,
            Err(error) => self.fail(ErrorStage::AudioStop, error.to_string()).await,
        }
    }

    async fn process_recording(&mut self, recording: RecordedAudio) {
        self.set_state(DictationState::Transcribing).await;
        let transcript = self.transcription.transcribe(&recording).await;
        let path = recording.path;
        let _ = tokio_fs::remove_file(&path).await;
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
                    Err(error) => self.fail(ErrorStage::Insertion, error.to_string()).await,
                }
            }
            Err(error) => {
                self.fail(ErrorStage::Transcription, error.to_string())
                    .await
            }
        }
    }

    async fn set_state(&mut self, state: DictationState) {
        self.state = state.clone();
        let _ = self.events.send(AppEvent::StateChanged(state)).await;
    }

    async fn fail(&mut self, stage: ErrorStage, message: String) {
        let _ = self.events.send(AppEvent::Error { stage, message }).await;
        self.set_state(DictationState::Idle).await;
    }
}

/// Drives the engine from a hotkey-event channel on its own task, so the
/// caller's event loop (e.g. a `tokio::select!` driving a tray) never blocks
/// on transcription or insertion. Hotkey events that arrive while a
/// transition is in flight are discarded once it completes, preserving the
/// same "ignore hotkeys outside the expected state" semantics as inline
/// `handle_hotkey` calls.
pub async fn run_engine_loop<A, T, I>(
    mut engine: DictationEngine<A, T, I>,
    mut hotkeys: mpsc::Receiver<HotkeyEvent>,
) where
    A: AudioRecorder,
    T: TranscriptionClient,
    I: TextInjector,
{
    while let Some(event) = hotkeys.recv().await {
        engine.handle_hotkey(event).await;
        while hotkeys.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioError, AudioResult, InsertMethod, PlatformError, PlatformResult, RecordedAudio,
        TranscriptionError, TranscriptionResult,
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
        stop_error: bool,
    }

    impl FakeAudio {
        fn ok(recording: Option<RecordedAudio>) -> Self {
            Self {
                recording: Mutex::new(recording),
                start_error: false,
                stop_error: false,
            }
        }

        fn start_failure() -> Self {
            Self {
                recording: Mutex::new(None),
                start_error: true,
                stop_error: false,
            }
        }

        fn stop_failure(recording: Option<RecordedAudio>) -> Self {
            Self {
                recording: Mutex::new(recording),
                start_error: false,
                stop_error: true,
            }
        }
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
            if self.stop_error {
                return Err(AudioError::Message("stop boom".into()));
            }
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

    struct FakeTranscription {
        text: String,
        error: Option<String>,
        delay: Option<Duration>,
    }

    impl FakeTranscription {
        fn ok(text: impl Into<String>) -> Self {
            Self {
                text: text.into(),
                error: None,
                delay: None,
            }
        }

        fn err(message: impl Into<String>) -> Self {
            Self {
                text: String::new(),
                error: Some(message.into()),
                delay: None,
            }
        }

        /// Makes `transcribe` sleep for `delay` before resolving, so tests can
        /// observe engine behavior while a transition is still in flight.
        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = Some(delay);
            self
        }
    }

    #[async_trait]
    impl TranscriptionClient for FakeTranscription {
        async fn transcribe(&self, _recording: &RecordedAudio) -> TranscriptionResult<String> {
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(message) = &self.error {
                return Err(TranscriptionError::Message(message.clone()));
            }
            Ok(self.text.clone())
        }
    }

    /// Returns the same successful recording on every `stop()` call (unlike
    /// `FakeAudio`, which consumes its recording after one use), so tests can
    /// drive multiple sequential dictation cycles through the same audio fake.
    struct RepeatingAudio {
        recording: RecordedAudio,
    }

    #[async_trait]
    impl AudioRecorder for RepeatingAudio {
        async fn start(&self) -> AudioResult<()> {
            Ok(())
        }

        async fn stop(&self) -> AudioResult<Option<RecordedAudio>> {
            Ok(Some(self.recording.clone()))
        }

        async fn cancel(&self) -> AudioResult<()> {
            Ok(())
        }
    }

    struct FakeInjector {
        insert_error: bool,
        calls: Mutex<usize>,
    }

    impl FakeInjector {
        fn ok() -> Self {
            Self {
                insert_error: false,
                calls: Mutex::new(0),
            }
        }

        fn failing() -> Self {
            Self {
                insert_error: true,
                calls: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl TextInjector for FakeInjector {
        async fn insert_text(&self, _text: &str) -> PlatformResult<InsertReport> {
            *self.calls.lock().unwrap() += 1;
            if self.insert_error {
                return Err(PlatformError::Message("insert boom".into()));
            }
            Ok(InsertReport {
                method: InsertMethod::ClipboardPaste,
                restored_clipboard: true,
            })
        }
    }

    fn recording_with_duration(duration: Duration) -> (RecordedAudio, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let recording = RecordedAudio {
            path: file.path().to_path_buf(),
            content_type: "audio/wav".into(),
            filename: "recording.wav".into(),
            duration,
        };
        (recording, file)
    }

    fn drain_events(rx: &mut mpsc::Receiver<AppEvent>) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn discards_short_recordings() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(20));
        let audio = Arc::new(FakeAudio::ok(Some(recording)));
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("ignored")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        engine.handle_hotkey(HotkeyEvent::Released).await;

        let events = drain_events(&mut rx);
        assert!(events
            .iter()
            .any(|event| matches!(event, AppEvent::RecordingDiscarded { .. })));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn returns_to_idle_after_error() {
        let audio = Arc::new(FakeAudio::start_failure());
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("ignored")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;

        let events = drain_events(&mut rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::AudioStart,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn speak_selection_hotkey_does_not_change_dictation_state() {
        let audio = Arc::new(FakeAudio::ok(None));
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("ignored")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::SpeakSelection).await;

        assert!(rx.try_recv().is_err());
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn audio_start_failure_returns_to_idle_with_audio_start_stage() {
        let audio = Arc::new(FakeAudio::start_failure());
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("ignored")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        let events = drain_events(&mut rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::AudioStart,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);

        // A second press proves the engine isn't wedged outside `Idle`: it attempts
        // `start()` again (and fails again) instead of silently ignoring the hotkey.
        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        let events = drain_events(&mut rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::AudioStart,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn audio_stop_failure_returns_to_idle_with_audio_stop_stage() {
        let audio = Arc::new(FakeAudio::stop_failure(None));
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("ignored")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        assert_eq!(engine.state(), &DictationState::Recording);

        engine.handle_hotkey(HotkeyEvent::Released).await;
        let events = drain_events(&mut rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::AudioStop,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn transcription_failure_deletes_recording_and_returns_to_idle() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(500));
        let recording_path = recording.path.clone();
        let audio = Arc::new(FakeAudio::ok(Some(recording)));
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::err("transcription boom")),
            Arc::new(FakeInjector::ok()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        engine.handle_hotkey(HotkeyEvent::Released).await;

        let events = drain_events(&mut rx);
        assert!(events.iter().any(
            |event| matches!(event, AppEvent::RecordingDeleted { path } if path == &recording_path)
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::Transcription,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn insertion_failure_returns_to_idle_with_insertion_stage() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(500));
        let audio = Arc::new(FakeAudio::ok(Some(recording)));
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("hello world")),
            Arc::new(FakeInjector::failing()),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        engine.handle_hotkey(HotkeyEvent::Released).await;

        let events = drain_events(&mut rx);
        assert!(events
            .iter()
            .any(|event| matches!(event, AppEvent::TranscriptReady { .. })));
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::Error {
                stage: ErrorStage::Insertion,
                ..
            }
        )));
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn empty_transcript_returns_to_idle_without_insertion() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(500));
        let audio = Arc::new(FakeAudio::ok(Some(recording)));
        let injector = Arc::new(FakeInjector::ok());
        let (tx, mut rx) = mpsc::channel(8);
        let mut engine = DictationEngine::new(
            audio,
            Arc::new(FakeTranscription::ok("   \n\t  ")),
            injector.clone(),
            tx,
        );

        engine.handle_hotkey(HotkeyEvent::Pressed).await;
        engine.handle_hotkey(HotkeyEvent::Released).await;

        let events = drain_events(&mut rx);
        assert!(!events
            .iter()
            .any(|event| matches!(event, AppEvent::TranscriptReady { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AppEvent::Inserted(..))));
        assert_eq!(injector.call_count(), 0);
        assert_eq!(engine.state(), &DictationState::Idle);
    }

    #[tokio::test]
    async fn engine_loop_discards_hotkeys_queued_during_transition() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(500));
        let audio = Arc::new(FakeAudio::ok(Some(recording)));
        let transcription =
            Arc::new(FakeTranscription::ok("hello").with_delay(Duration::from_millis(40)));
        let injector = Arc::new(FakeInjector::ok());
        let (tx, mut rx) = mpsc::channel(8);
        let engine = DictationEngine::new(audio, transcription, injector, tx);
        let (hotkey_tx, hotkey_rx) = mpsc::channel(16);

        let handle = tokio::spawn(run_engine_loop(engine, hotkey_rx));

        hotkey_tx.send(HotkeyEvent::Pressed).await.unwrap();
        // Let Pressed settle into Recording before Released is queued.
        tokio::time::sleep(Duration::from_millis(20)).await;
        hotkey_tx.send(HotkeyEvent::Released).await.unwrap();
        // Let Released begin the (delayed) transcription, then queue a Pressed
        // while the 40ms delay is still in flight.
        tokio::time::sleep(Duration::from_millis(15)).await;
        hotkey_tx.send(HotkeyEvent::Pressed).await.unwrap();

        // Let the delay resolve, the transition finish, and the post-handle
        // drain discard the queued Pressed.
        tokio::time::sleep(Duration::from_millis(150)).await;
        drop(hotkey_tx);
        handle.await.unwrap();

        let events = drain_events(&mut rx);
        let recording_starts = events
            .iter()
            .filter(|event| matches!(event, AppEvent::StateChanged(DictationState::Recording)))
            .count();
        assert_eq!(
            recording_starts, 1,
            "trailing Pressed queued during transcription must be discarded"
        );
        assert_eq!(
            events.last(),
            Some(&AppEvent::StateChanged(DictationState::Idle))
        );
    }

    #[tokio::test]
    async fn engine_loop_processes_sequential_dictations() {
        let (recording, _file) = recording_with_duration(Duration::from_millis(500));
        let audio = Arc::new(RepeatingAudio { recording });
        let transcription = Arc::new(FakeTranscription::ok("hello world"));
        let injector = Arc::new(FakeInjector::ok());
        let (tx, mut rx) = mpsc::channel(16);
        let engine = DictationEngine::new(audio, transcription, injector.clone(), tx);
        let (hotkey_tx, hotkey_rx) = mpsc::channel(16);

        let handle = tokio::spawn(run_engine_loop(engine, hotkey_rx));

        for _ in 0..2 {
            hotkey_tx.send(HotkeyEvent::Pressed).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            hotkey_tx.send(HotkeyEvent::Released).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        drop(hotkey_tx);
        handle.await.unwrap();

        let events = drain_events(&mut rx);
        let inserted = events
            .iter()
            .filter(|event| matches!(event, AppEvent::Inserted(..)))
            .count();
        assert_eq!(inserted, 2, "expected two complete dictation flows");
        assert_eq!(injector.call_count(), 2);
        assert_eq!(
            events.last(),
            Some(&AppEvent::StateChanged(DictationState::Idle))
        );
    }
}
