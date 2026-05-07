use async_trait::async_trait;
use codex_voice_core::{AudioError, AudioRecorder, AudioResult, RecordedAudio};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, Stream,
};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::{
    fs::File,
    io::BufWriter,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type Writer = WavWriter<BufWriter<File>>;

#[derive(Default)]
pub struct CpalWavRecorder {
    state: Mutex<Option<CaptureState>>,
}

struct CaptureState {
    stream: Stream,
    writer: Arc<Mutex<Option<Writer>>>,
    path: PathBuf,
    started_at: Instant,
    samples_written: Arc<Mutex<u64>>,
    sample_rate: u32,
}

// CPAL marks Stream as not sendable across every backend it supports. This
// recorder never shares the stream with audio callbacks; callbacks only receive
// the writer and counter Arcs. The stream is created, paused, and dropped through
// the recorder state, so the assertion is kept narrow to host targets we validate.
#[cfg(any(target_os = "linux", target_os = "windows"))]
unsafe impl Send for CaptureState {}

impl CpalWavRecorder {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AudioRecorder for CpalWavRecorder {
    async fn start(&self) -> AudioResult<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AudioError::Message("audio state lock poisoned".into()))?;
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
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let path = tempfile::Builder::new()
            .prefix("codex-voice-")
            .suffix(".wav")
            .tempfile()
            .map_err(|error| AudioError::Message(format!("failed to create temp wav: {error}")))?
            .into_temp_path()
            .keep()
            .map_err(|error| AudioError::Message(format!("failed to persist temp wav: {error}")))?;

        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: WavSampleFormat::Int,
        };
        let writer = Arc::new(Mutex::new(Some(WavWriter::create(&path, spec).map_err(
            |error| {
                let _ = std::fs::remove_file(&path);
                AudioError::Message(format!("failed to create wav: {error}"))
            },
        )?)));
        let samples_written = Arc::new(Mutex::new(0));
        let writer_for_stream = Arc::clone(&writer);
        let count_for_stream = Arc::clone(&samples_written);
        let err_fn = |error| tracing::error!("audio input stream error: {error}");

        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| {
                    write_f32(data, channels, &writer_for_stream, &count_for_stream)
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    write_i16(data, channels, &writer_for_stream, &count_for_stream)
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _| {
                    write_u16(data, channels, &writer_for_stream, &count_for_stream)
                },
                err_fn,
                None,
            ),
            other => {
                let _ = std::fs::remove_file(&path);
                return Err(AudioError::Message(format!(
                    "unsupported input sample format: {other:?}"
                )));
            }
        }
        .map_err(|error| {
            let _ = std::fs::remove_file(&path);
            AudioError::Message(format!("failed to build input stream: {error}"))
        })?;
        stream.play().map_err(|error| {
            let _ = std::fs::remove_file(&path);
            AudioError::Message(format!("failed to start input stream: {error}"))
        })?;

        *state = Some(CaptureState {
            stream,
            writer,
            path,
            started_at: Instant::now(),
            samples_written,
            sample_rate,
        });
        Ok(())
    }

    async fn stop(&self) -> AudioResult<Option<RecordedAudio>> {
        let capture = self
            .state
            .lock()
            .map_err(|_| AudioError::Message("audio state lock poisoned".into()))?
            .take();
        let Some(capture) = capture else {
            return Ok(None);
        };
        let _ = capture.stream.pause();
        std::thread::sleep(Duration::from_millis(50));
        let duration = capture.started_at.elapsed();
        let sample_count = *capture
            .samples_written
            .lock()
            .map_err(|_| AudioError::Message("audio sample counter lock poisoned".into()))?;
        if let Some(writer) = capture
            .writer
            .lock()
            .map_err(|_| AudioError::Message("audio writer lock poisoned".into()))?
            .take()
        {
            writer
                .finalize()
                .map_err(|error| AudioError::Message(format!("failed to finalize wav: {error}")))?;
        }
        drop(capture.stream);

        let duration = if sample_count > 0 {
            Duration::from_secs_f64(sample_count as f64 / capture.sample_rate as f64)
        } else {
            duration
        };
        Ok(Some(RecordedAudio {
            filename: capture
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("codex-voice.wav")
                .to_string(),
            path: capture.path,
            content_type: "audio/wav".into(),
            duration,
        }))
    }

    async fn cancel(&self) -> AudioResult<()> {
        if let Some(recording) = self.stop().await? {
            let _ = std::fs::remove_file(recording.path);
        }
        Ok(())
    }
}

fn write_f32(
    data: &[f32],
    channels: usize,
    writer: &Arc<Mutex<Option<Writer>>>,
    samples_written: &Arc<Mutex<u64>>,
) {
    write_interleaved_mono(data, channels, writer, samples_written, |sample| {
        (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
    });
}

fn write_i16(
    data: &[i16],
    channels: usize,
    writer: &Arc<Mutex<Option<Writer>>>,
    samples_written: &Arc<Mutex<u64>>,
) {
    write_interleaved_mono(data, channels, writer, samples_written, |sample| *sample);
}

fn write_u16(
    data: &[u16],
    channels: usize,
    writer: &Arc<Mutex<Option<Writer>>>,
    samples_written: &Arc<Mutex<u64>>,
) {
    write_interleaved_mono(data, channels, writer, samples_written, |sample| {
        (*sample as i32 - 32768) as i16
    });
}

fn write_interleaved_mono<T>(
    data: &[T],
    channels: usize,
    writer: &Arc<Mutex<Option<Writer>>>,
    samples_written: &Arc<Mutex<u64>>,
    to_i16: impl Fn(&T) -> i16,
) {
    let channels = channels.max(1);
    write_mono(
        data.chunks(channels).map(|frame| {
            let sum = frame
                .iter()
                .map(|sample| to_i16(sample) as i32)
                .sum::<i32>();
            (sum / frame.len().max(1) as i32) as i16
        }),
        writer,
        samples_written,
    );
}

fn write_mono(
    samples: impl Iterator<Item = i16>,
    writer: &Arc<Mutex<Option<Writer>>>,
    samples_written: &Arc<Mutex<u64>>,
) {
    let Ok(mut writer_guard) = writer.lock() else {
        return;
    };
    let Some(writer) = writer_guard.as_mut() else {
        return;
    };
    let mut written = 0;
    for sample in samples {
        if writer.write_sample(sample).is_ok() {
            written += 1;
        }
    }
    if let Ok(mut count) = samples_written.lock() {
        *count += written;
    }
}
