use std::{io, path::Path, time::Duration};

/// Read the duration of a WAV file from its header.
pub fn wav_duration(path: &Path) -> io::Result<Duration> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid wav: {e}")))?;
    let spec = reader.spec();
    let samples = reader.duration();
    let sample_rate = spec.sample_rate.max(1) as f64;
    Ok(Duration::from_secs_f64(samples as f64 / sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{WavSpec, WavWriter};

    fn write_silence(path: &Path, spec: WavSpec, sample_count: usize) {
        let mut writer = WavWriter::create(path, spec).unwrap();
        for _ in 0..sample_count {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn mono_wav_duration() {
        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        write_silence(&path, spec, 16_000);
        assert_eq!(wav_duration(&path).unwrap(), Duration::from_secs(1));
    }

    #[test]
    fn stereo_wav_duration_accounts_for_channels() {
        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let spec = WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        // 32_000 total samples across 2 channels = 16_000 frames = 1 second
        write_silence(&path, spec, 32_000);
        assert_eq!(wav_duration(&path).unwrap(), Duration::from_secs(1));
    }

    #[test]
    fn writer_thread_finalizes_only_after_senders_drop() {
        use crate::recorder::run_writer;

        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let (data_tx, data_rx) = crossbeam_channel::bounded::<Vec<i16>>(8);
        let (pool_tx, _pool_rx) = crossbeam_channel::bounded::<Vec<i16>>(8);
        let writer_path = path.clone();
        let handle = std::thread::spawn(move || run_writer(writer_path, spec, data_rx, pool_tx));

        data_tx.send(vec![1_i16, 2, 3, 4]).unwrap();
        // Dropping the last sender is what lets the writer exit its recv loop
        // and finalize the file.
        drop(data_tx);

        let count = handle.join().unwrap().unwrap();
        assert_eq!(count, 4);

        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 4);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn no_orphan_file_when_writer_never_spawned() {
        use crate::state::TempWavGuard;

        let path = tempfile::Builder::new()
            .prefix("codex-voice-")
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        assert!(path.exists());

        let guard = TempWavGuard::new(path.clone());
        // Mirrors a start() early-return before the writer thread is spawned:
        // the guard is the sole owner of the temp file and must delete it.
        drop(guard);

        assert!(
            !path.exists(),
            "temp file must be deleted when no writer runs"
        );
    }

    #[test]
    fn invalid_wav_returns_error() {
        let path = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        std::fs::write(&path, b"not a wav").unwrap();
        assert!(wav_duration(&path).is_err());
    }
}
