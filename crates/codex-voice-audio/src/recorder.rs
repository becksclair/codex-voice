use async_trait::async_trait;
use codex_voice_core::{AudioError, AudioRecorder, AudioResult, RecordedAudio};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat,
};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::sample::{write_f32, write_i16, write_u16};
use crate::state::{lock_or_poison, CaptureState, TempWavGuard};

#[derive(Default)]
pub struct CpalWavRecorder {
    state: Mutex<Option<CaptureState>>,
}

impl CpalWavRecorder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Drop for CpalWavRecorder {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(capture) = state.take() else {
            return;
        };
        let (writer_thread, path, _, _) = close_capture(capture);
        let _ = writer_thread.join();
        let _ = std::fs::remove_file(path);
    }
}

#[async_trait]
impl AudioRecorder for CpalWavRecorder {
    async fn start(&self) -> AudioResult<()> {
        let mut state = lock_or_poison(&self.state, "audio state")?;
        if state.is_some() {
            return Err(AudioError::Message("recording already in progress".into()));
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::Message("no default input device found".into()))?;
        let config = device.default_input_config().map_err(|error| {
            AudioError::Message(format!("failed to read input config: {error}"))
        })?;
        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let path = tempfile::Builder::new()
            .prefix("codex-voice-")
            .suffix(".wav")
            .tempfile()
            .map_err(|error| AudioError::Message(format!("failed to create temp wav: {error}")))?
            .into_temp_path()
            .keep()
            .map_err(|error| AudioError::Message(format!("failed to persist temp wav: {error}")))?;
        let guard = TempWavGuard::new(path.clone());

        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: WavSampleFormat::Int,
        };
        let writer_path = guard.path().clone();

        // Data channel for filled audio chunks; pool channel for reusable empty buffers.
        let (data_tx, data_rx) = crossbeam_channel::bounded::<Vec<i16>>(2048);
        let (pool_tx, pool_rx) = crossbeam_channel::bounded(512);
        for _ in 0..512 {
            let _ = pool_tx.try_send(Vec::with_capacity(1024));
        }

        let data_tx_f32 = data_tx.clone();
        let pool_rx_f32 = pool_rx.clone();
        let pool_tx_f32 = pool_tx.clone();
        let data_tx_i16 = data_tx.clone();
        let pool_rx_i16 = pool_rx.clone();
        let pool_tx_i16 = pool_tx.clone();
        let data_tx_u16 = data_tx.clone();
        let pool_rx_u16 = pool_rx.clone();
        let pool_tx_u16 = pool_tx.clone();
        let err_fn = |error| tracing::error!("audio input stream error: {error}");

        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    write_f32(data, channels, &data_tx_f32, &pool_rx_f32, &pool_tx_f32);
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    write_i16(data, channels, &data_tx_i16, &pool_rx_i16, &pool_tx_i16);
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _| {
                    write_u16(data, channels, &data_tx_u16, &pool_rx_u16, &pool_tx_u16);
                },
                err_fn,
                None,
            ),
            other => {
                return Err(AudioError::Message(format!(
                    "unsupported input sample format: {other:?}"
                )));
            }
        }
        .map_err(|error| AudioError::Message(format!("failed to build input stream: {error}")))?;
        stream.play().map_err(|error| {
            AudioError::Message(format!("failed to start input stream: {error}"))
        })?;

        // Spawn the writer thread only after the stream is playing. Any early
        // return above leaves no thread running, so the TempWavGuard's Drop is
        // the sole owner of the temp file and deletes it cleanly. The bounded
        // data channel buffers the few chunks cpal may push before this starts.
        let writer_thread =
            std::thread::spawn(move || run_writer(writer_path, spec, data_rx, pool_tx));

        let _ = guard.keep();
        *state = Some(CaptureState {
            stream,
            data_tx,
            writer_thread: Some(writer_thread),
            path,
            started_at: Instant::now(),
            sample_rate,
        });
        Ok(())
    }

    async fn stop(&self) -> AudioResult<Option<RecordedAudio>> {
        let capture = lock_or_poison(&self.state, "audio state")?.take();
        let Some(capture) = capture else {
            return Ok(None);
        };
        let (writer_thread, path, duration, sample_rate) = close_capture(capture);

        let sample_count = tokio::task::spawn_blocking(move || {
            writer_thread
                .join()
                .map_err(|_| AudioError::Message("wav writer thread panicked".into()))?
        })
        .await
        .map_err(|error| {
            AudioError::Message(format!("wav writer thread join failed: {error}"))
        })??;

        let duration = if sample_count > 0 {
            Duration::from_secs_f64(sample_count as f64 / sample_rate.max(1) as f64)
        } else {
            duration
        };
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("codex-voice.wav")
            .to_string();
        Ok(Some(RecordedAudio {
            filename,
            path,
            content_type: "audio/wav".into(),
            duration,
        }))
    }

    async fn cancel(&self) -> AudioResult<()> {
        if let Some(recording) = self.stop().await? {
            let _ = tokio::fs::remove_file(recording.path).await;
        }
        Ok(())
    }
}

/// Drain filled audio chunks into a WAV file until every sender is dropped,
/// then finalize the file. Returns the total number of samples written.
pub(crate) fn run_writer(
    path: PathBuf,
    spec: WavSpec,
    data_rx: crossbeam_channel::Receiver<Vec<i16>>,
    pool_tx: crossbeam_channel::Sender<Vec<i16>>,
) -> AudioResult<u64> {
    let mut writer = WavWriter::create(&path, spec)
        .map_err(|error| AudioError::Message(format!("failed to create wav: {error}")))?;
    let mut count = 0u64;
    while let Ok(mut chunk) = data_rx.recv() {
        for sample in chunk.drain(..) {
            let _ = writer.write_sample(sample);
            count += 1;
        }
        let _ = pool_tx.try_send(chunk);
    }
    writer
        .finalize()
        .map_err(|error| AudioError::Message(format!("failed to finalize wav: {error}")))?;
    Ok(count)
}

fn close_capture(
    capture: CaptureState,
) -> (
    std::thread::JoinHandle<AudioResult<u64>>,
    std::path::PathBuf,
    Duration,
    u32,
) {
    let CaptureState {
        stream,
        data_tx,
        writer_thread,
        path,
        started_at,
        sample_rate,
    } = capture;

    // Drop the stream before joining the writer so callback-owned sender clones close too.
    let _ = stream.pause();
    drop(stream);
    drop(data_tx);

    (
        writer_thread.expect("writer thread is always present"),
        path,
        started_at.elapsed(),
        sample_rate,
    )
}
