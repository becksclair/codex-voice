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
