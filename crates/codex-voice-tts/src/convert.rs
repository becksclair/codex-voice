use bytes::Bytes;
use std::io::Cursor;
use std::process::Stdio;

use codex_voice_core::{SpeechError, SpeechFormat, SpeechResult, SynthesizedSpeech};
use tokio::{io::AsyncWriteExt, process::Command, time};

const FFMPEG_CONVERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const CHUNK_BOUNDARY_SILENCE_MS: u32 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PcmSpec {
    sample_rate: u32,
    channels: u16,
}

pub async fn convert_speech(
    speech: SynthesizedSpeech,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    if speech.format == target {
        return Ok(speech);
    }

    match (speech.format, target) {
        (SpeechFormat::Pcm, SpeechFormat::Wav) => pcm_to_wav_blocking(speech).await,
        (SpeechFormat::Pcm, target) => convert_pcm_with_ffmpeg(speech, target).await,
        (_, target) => convert_encoded_with_ffmpeg(speech, target).await,
    }
}

pub async fn concatenate_wav_chunks(
    chunks: Vec<SynthesizedSpeech>,
) -> SpeechResult<SynthesizedSpeech> {
    tokio::task::spawn_blocking(move || concatenate_wav_chunks_blocking(chunks))
        .await
        .map_err(|e| SpeechError::Request(format!("WAV concat task failed: {e}")))?
}

pub async fn concatenate_pcm_chunks(
    chunks: Vec<SynthesizedSpeech>,
) -> SpeechResult<SynthesizedSpeech> {
    tokio::task::spawn_blocking(move || concatenate_pcm_chunks_blocking(chunks))
        .await
        .map_err(|e| SpeechError::Request(format!("PCM concat task failed: {e}")))?
}

fn concatenate_wav_chunks_blocking(
    chunks: Vec<SynthesizedSpeech>,
) -> SpeechResult<SynthesizedSpeech> {
    let first = chunks
        .first()
        .ok_or_else(|| SpeechError::Request("cannot concatenate zero WAV chunks".into()))?;
    if first.format != SpeechFormat::Wav {
        return Err(SpeechError::Request(
            "cannot concatenate non-WAV speech chunks".into(),
        ));
    }
    let spec = hound::WavReader::new(Cursor::new(first.bytes.clone()))
        .map_err(|e| SpeechError::Request(format!("failed to read first WAV chunk: {e}")))?
        .spec();

    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err(SpeechError::Request(format!(
            "cannot concatenate WAV chunks with unsupported sample format: {}-bit {:?}",
            spec.bits_per_sample, spec.sample_format
        )));
    }

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(|e| {
            SpeechError::Request(format!("failed to create WAV concat writer: {e}"))
        })?;
        for (index, chunk) in chunks.into_iter().enumerate() {
            if chunk.format != SpeechFormat::Wav {
                return Err(SpeechError::Request(format!(
                    "cannot concatenate non-WAV speech chunk {index}"
                )));
            }
            let mut reader = hound::WavReader::new(Cursor::new(chunk.bytes)).map_err(|e| {
                SpeechError::Request(format!("failed to read WAV chunk {index}: {e}"))
            })?;
            if reader.spec() != spec {
                return Err(SpeechError::Request(format!(
                    "cannot concatenate WAV chunk {index} with mismatched spec"
                )));
            }
            if index > 0 {
                write_wav_silence(&mut writer, spec, index)?;
            }
            for sample in reader.samples::<i16>() {
                writer
                    .write_sample(sample.map_err(|e| {
                        SpeechError::Request(format!("failed to read WAV chunk {index}: {e}"))
                    })?)
                    .map_err(|e| {
                        SpeechError::Request(format!("failed to write WAV chunk {index}: {e}"))
                    })?;
            }
        }
        writer
            .finalize()
            .map_err(|e| SpeechError::Request(format!("failed to finalize WAV concat: {e}")))?;
    }

    Ok(SynthesizedSpeech {
        bytes: Bytes::from(cursor.into_inner()),
        format: SpeechFormat::Wav,
        mime_type: SpeechFormat::Wav.mime_type().to_string(),
        prepared_input: None,
    })
}

fn write_wav_silence<W: std::io::Write + std::io::Seek>(
    writer: &mut hound::WavWriter<W>,
    spec: hound::WavSpec,
    chunk_index: usize,
) -> SpeechResult<()> {
    let frames = (u64::from(spec.sample_rate) * u64::from(CHUNK_BOUNDARY_SILENCE_MS)) / 1_000;
    let samples = frames.saturating_mul(u64::from(spec.channels));
    for _ in 0..samples {
        writer.write_sample(0_i16).map_err(|e| {
            SpeechError::Request(format!(
                "failed to write WAV boundary silence before chunk {chunk_index}: {e}"
            ))
        })?;
    }
    Ok(())
}

fn concatenate_pcm_chunks_blocking(
    chunks: Vec<SynthesizedSpeech>,
) -> SpeechResult<SynthesizedSpeech> {
    let first = chunks
        .first()
        .ok_or_else(|| SpeechError::Request("cannot concatenate zero PCM chunks".into()))?;
    if first.format != SpeechFormat::Pcm {
        return Err(SpeechError::Request(
            "cannot concatenate non-PCM speech chunks".into(),
        ));
    }

    let spec = parse_pcm_spec(&first.mime_type);
    let silence = boundary_silence_bytes(spec);
    let capacity = chunks.iter().map(|chunk| chunk.bytes.len()).sum::<usize>()
        + silence.len().saturating_mul(chunks.len().saturating_sub(1));
    let mut bytes = Vec::with_capacity(capacity);
    let mime_type = first.mime_type.clone();

    for (index, chunk) in chunks.into_iter().enumerate() {
        if chunk.format != SpeechFormat::Pcm {
            return Err(SpeechError::Request(format!(
                "cannot concatenate non-PCM speech chunk {index}"
            )));
        }
        if parse_pcm_spec(&chunk.mime_type) != spec {
            return Err(SpeechError::Request(format!(
                "cannot concatenate PCM chunk {index} with mismatched spec"
            )));
        }
        if index > 0 {
            bytes.extend_from_slice(&silence);
        }
        bytes.extend_from_slice(&chunk.bytes);
    }

    pcm_to_wav(SynthesizedSpeech {
        bytes: Bytes::from(bytes),
        format: SpeechFormat::Pcm,
        mime_type,
        prepared_input: None,
    })
}

fn boundary_silence_bytes(spec: PcmSpec) -> Vec<u8> {
    let frames = (u64::from(spec.sample_rate) * u64::from(CHUNK_BOUNDARY_SILENCE_MS)) / 1_000;
    let bytes = frames
        .saturating_mul(u64::from(spec.channels))
        .saturating_mul(2)
        .min(usize::MAX as u64) as usize;
    vec![0; bytes]
}

async fn pcm_to_wav_blocking(speech: SynthesizedSpeech) -> SpeechResult<SynthesizedSpeech> {
    tokio::task::spawn_blocking(move || pcm_to_wav(speech))
        .await
        .map_err(|e| SpeechError::Request(format!("PCM to WAV conversion task failed: {e}")))?
}

fn pcm_to_wav(speech: SynthesizedSpeech) -> SpeechResult<SynthesizedSpeech> {
    let prepared_input = speech.prepared_input;
    let spec = parse_pcm_spec(&speech.mime_type);
    let mut cursor = Cursor::new(Vec::with_capacity(speech.bytes.len() + 44));
    let wav_spec = hound::WavSpec {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    {
        let mut writer = hound::WavWriter::new(&mut cursor, wav_spec)
            .map_err(|e| SpeechError::Request(format!("failed to create WAV writer: {e}")))?;
        for sample in speech.bytes.chunks_exact(2) {
            // Google returns little-endian 16-bit PCM despite the L16 media type name.
            writer
                .write_sample(i16::from_le_bytes([sample[0], sample[1]]))
                .map_err(|e| SpeechError::Request(format!("failed to write WAV sample: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| SpeechError::Request(format!("failed to finalize WAV: {e}")))?;
    }

    Ok(SynthesizedSpeech {
        bytes: Bytes::from(cursor.into_inner()),
        format: SpeechFormat::Wav,
        mime_type: SpeechFormat::Wav.mime_type().to_string(),
        prepared_input,
    })
}

async fn convert_encoded_with_ffmpeg(
    speech: SynthesizedSpeech,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    run_ffmpeg(
        speech.bytes,
        vec!["-i", "pipe:0"],
        target,
        speech.prepared_input,
    )
    .await
}

async fn convert_pcm_with_ffmpeg(
    speech: SynthesizedSpeech,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    let spec = parse_pcm_spec(&speech.mime_type);
    let sample_rate = spec.sample_rate.to_string();
    let channels = spec.channels.to_string();
    let input_args = vec![
        "-f",
        "s16le",
        "-ar",
        &sample_rate,
        "-ac",
        &channels,
        "-i",
        "pipe:0",
    ];
    run_ffmpeg(speech.bytes, input_args, target, speech.prepared_input).await
}

async fn run_ffmpeg(
    input: Bytes,
    input_args: Vec<&str>,
    target: SpeechFormat,
    prepared_input: Option<String>,
) -> SpeechResult<SynthesizedSpeech> {
    let output_args = ffmpeg_output_args(target);
    let mut command = Command::new("ffmpeg");
    command
        .kill_on_drop(true)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .args(input_args)
        .args(output_args.iter().copied())
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        SpeechError::Unavailable(format!(
            "ffmpeg is required to convert TTS audio to {}; failed to spawn: {e}",
            target.to_openai()
        ))
    })?;

    let output = time::timeout(FFMPEG_CONVERSION_TIMEOUT, async move {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            SpeechError::Request("failed to open ffmpeg stdin for TTS conversion".into())
        })?;
        let write_input = async move {
            stdin.write_all(&input).await.map_err(|e| {
                SpeechError::Request(format!("failed to write TTS audio to ffmpeg: {e}"))
            })?;
            drop(stdin);
            Ok::<(), SpeechError>(())
        };
        let wait_output = async move {
            child.wait_with_output().await.map_err(|e| {
                SpeechError::Request(format!("failed to wait for ffmpeg conversion: {e}"))
            })
        };

        let (_, output) = tokio::try_join!(write_input, wait_output)?;
        Ok::<_, SpeechError>(output)
    })
    .await
    .map_err(|_| {
        SpeechError::Request(format!(
            "ffmpeg timed out converting TTS audio to {} after {}s",
            target.to_openai(),
            FFMPEG_CONVERSION_TIMEOUT.as_secs()
        ))
    })??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SpeechError::Request(format!(
            "ffmpeg failed to convert TTS audio to {}: {}",
            target.to_openai(),
            stderr.trim()
        )));
    }

    if output.stdout.is_empty() {
        return Err(SpeechError::Request(format!(
            "ffmpeg produced empty {} audio",
            target.to_openai()
        )));
    }

    Ok(SynthesizedSpeech {
        bytes: Bytes::from(output.stdout),
        format: target,
        mime_type: target.mime_type().to_string(),
        prepared_input,
    })
}

fn ffmpeg_output_args(target: SpeechFormat) -> &'static [&'static str] {
    match target {
        SpeechFormat::Mp3 => &["-f", "mp3"],
        SpeechFormat::Opus => &["-c:a", "libopus", "-f", "opus"],
        SpeechFormat::Aac => &["-c:a", "aac", "-f", "adts"],
        SpeechFormat::Flac => &["-f", "flac"],
        SpeechFormat::Wav => &["-f", "wav"],
        SpeechFormat::Pcm => &["-f", "s16le"],
    }
}

fn parse_pcm_spec(mime_type: &str) -> PcmSpec {
    let mut sample_rate = 24_000;
    let mut channels = 1;

    for param in mime_type.split(';').skip(1) {
        let Some((key, value)) = param.trim().split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "rate" => {
                if let Ok(parsed) = value.trim().parse() {
                    sample_rate = parsed;
                }
            }
            "channels" => {
                if let Ok(parsed) = value.trim().parse() {
                    channels = parsed;
                }
            }
            _ => {}
        }
    }

    PcmSpec {
        sample_rate,
        channels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wav(samples: &[i16]) -> SynthesizedSpeech {
        let mut cursor = Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        SynthesizedSpeech {
            bytes: Bytes::from(cursor.into_inner()),
            format: SpeechFormat::Wav,
            mime_type: "audio/wav".to_string(),
            prepared_input: None,
        }
    }

    fn test_pcm(samples: &[i16]) -> SynthesizedSpeech {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        SynthesizedSpeech {
            bytes: Bytes::from(bytes),
            format: SpeechFormat::Pcm,
            mime_type: "audio/L16;codec=pcm;rate=24000".to_string(),
            prepared_input: None,
        }
    }

    fn read_wav_samples(speech: SynthesizedSpeech) -> Vec<i16> {
        let mut reader = hound::WavReader::new(Cursor::new(speech.bytes)).unwrap();
        reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[tokio::test]
    async fn wraps_pcm_as_wav() {
        let speech = SynthesizedSpeech {
            bytes: Bytes::from_static(&[0, 0, 1, 0, 255, 255, 0, 0]),
            format: SpeechFormat::Pcm,
            mime_type: "audio/L16;codec=pcm;rate=24000".to_string(),
            prepared_input: Some("[softly] hello".to_string()),
        };

        let converted = convert_speech(speech, SpeechFormat::Wav).await.unwrap();
        assert_eq!(converted.format, SpeechFormat::Wav);
        assert_eq!(converted.mime_type, "audio/wav");
        assert_eq!(converted.prepared_input.as_deref(), Some("[softly] hello"));
        assert_eq!(&converted.bytes[..4], b"RIFF");
        assert_eq!(&converted.bytes[8..12], b"WAVE");
    }

    #[tokio::test]
    async fn concatenates_wav_chunks_with_boundary_silence() {
        let combined = concatenate_wav_chunks(vec![test_wav(&[1, 2]), test_wav(&[3, 4])])
            .await
            .unwrap();
        let samples = read_wav_samples(combined.clone());
        let silence_samples = (24_000 * CHUNK_BOUNDARY_SILENCE_MS / 1_000) as usize;

        assert_eq!(combined.format, SpeechFormat::Wav);
        assert_eq!(&samples[..2], &[1, 2]);
        assert!(samples[2..2 + silence_samples]
            .iter()
            .all(|sample| *sample == 0));
        assert_eq!(&samples[2 + silence_samples..], &[3, 4]);
    }

    #[tokio::test]
    async fn concatenates_pcm_chunks_with_boundary_silence() {
        let combined = concatenate_pcm_chunks(vec![test_pcm(&[1, 2]), test_pcm(&[3, 4])])
            .await
            .unwrap();
        let samples = read_wav_samples(combined.clone());
        let silence_samples = (24_000 * CHUNK_BOUNDARY_SILENCE_MS / 1_000) as usize;

        assert_eq!(combined.format, SpeechFormat::Wav);
        assert_eq!(&samples[..2], &[1, 2]);
        assert!(samples[2..2 + silence_samples]
            .iter()
            .all(|sample| *sample == 0));
        assert_eq!(&samples[2 + silence_samples..], &[3, 4]);
    }
}
