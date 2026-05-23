use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    thread::JoinHandle,
    time::Instant,
};

use cpal::Stream;
use crossbeam_channel::Sender;

use codex_voice_core::{AudioError, AudioResult};

pub(crate) struct CaptureState {
    pub(crate) stream: Stream,
    pub(crate) data_tx: Sender<Vec<i16>>,
    pub(crate) writer_thread: Option<JoinHandle<AudioResult<u64>>>,
    pub(crate) path: PathBuf,
    pub(crate) started_at: Instant,
    pub(crate) sample_rate: u32,
}

// CPAL marks Stream as not sendable across every backend it supports. This
// recorder never shares the stream with audio callbacks; callbacks only receive
// the sender clone. The stream is created, paused, and dropped through the
// recorder state, so the assertion is kept narrow to host targets we validate.
#[cfg(any(target_os = "linux", target_os = "windows"))]
unsafe impl Send for CaptureState {}

/// RAII guard that deletes a temporary WAV file on drop unless explicitly kept.
pub(crate) struct TempWavGuard(Option<PathBuf>);

impl TempWavGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    /// Consume the guard and return the underlying path, preventing deletion.
    pub(crate) fn keep(mut self) -> PathBuf {
        self.0
            .take()
            .expect("TempWavGuard path is always set until keep()")
    }

    pub(crate) fn path(&self) -> &PathBuf {
        self.0
            .as_ref()
            .expect("TempWavGuard path is always set until keep()")
    }
}

impl Drop for TempWavGuard {
    fn drop(&mut self) {
        if let Some(ref path) = self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn lock_or_poison<'a, T>(
    mutex: &'a Mutex<T>,
    context: &str,
) -> AudioResult<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| AudioError::Message(format!("{context} lock poisoned")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_wav_guard_deletes_on_drop() {
        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        assert!(path.exists());
        {
            let guard = TempWavGuard::new(path.clone());
            assert_eq!(guard.path(), &path);
            drop(guard);
        }
        assert!(!path.exists(), "file should be deleted on drop");
    }

    #[test]
    fn temp_wav_guard_keep_prevents_deletion() {
        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        assert!(path.exists());
        {
            let guard = TempWavGuard::new(path.clone());
            let kept = guard.keep();
            assert_eq!(kept, path);
            // guard dropped here, but keep() took the path so nothing to delete
        }
        assert!(path.exists(), "file should survive after keep()");
        std::fs::remove_file(&path).unwrap();
    }
}
