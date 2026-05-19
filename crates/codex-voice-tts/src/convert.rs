use std::io::Cursor;
use std::process::Stdio;

use codex_voice_core::{SpeechError, SpeechFormat, SpeechResult, SynthesizedSpeech};
use tokio::{io::AsyncWriteExt, process::Command, time};

const FFMPEG_CONVERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone, Copy)]
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

async fn pcm_to_wav_blocking(speech: SynthesizedSpeech) -> SpeechResult<SynthesizedSpeech> {
    tokio::task::spawn_blocking(move || pcm_to_wav(speech))
        .await
        .map_err(|e| SpeechError::Request(format!("PCM to WAV conversion task failed: {e}")))?
}

fn pcm_to_wav(speech: SynthesizedSpeech) -> SpeechResult<SynthesizedSpeech> {
    let spec = parse_pcm_spec(&speech.mime_type);
    let mut cursor = Cursor::new(Vec::new());
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
        bytes: cursor.into_inner(),
        format: SpeechFormat::Wav,
        mime_type: SpeechFormat::Wav.mime_type().to_string(),
    })
}

async fn convert_pcm_with_ffmpeg(
    speech: SynthesizedSpeech,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    let spec = parse_pcm_spec(&speech.mime_type);
    let input_args = vec![
        "-f".to_string(),
        "s16le".to_string(),
        "-ar".to_string(),
        spec.sample_rate.to_string(),
        "-ac".to_string(),
        spec.channels.to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
    ];
    run_ffmpeg(speech.bytes, input_args, target).await
}

async fn convert_encoded_with_ffmpeg(
    speech: SynthesizedSpeech,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    run_ffmpeg(
        speech.bytes,
        vec!["-i".to_string(), "pipe:0".to_string()],
        target,
    )
    .await
}

async fn run_ffmpeg(
    input: Vec<u8>,
    input_args: Vec<String>,
    target: SpeechFormat,
) -> SpeechResult<SynthesizedSpeech> {
    let output_args = ffmpeg_output_args(target);
    let mut command = Command::new("ffmpeg");
    command
        .kill_on_drop(true)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .args(input_args)
        .args(output_args)
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

    let mut stdin = child.stdin.take().ok_or_else(|| {
        SpeechError::Request("failed to open ffmpeg stdin for TTS conversion".into())
    })?;
    stdin
        .write_all(&input)
        .await
        .map_err(|e| SpeechError::Request(format!("failed to write TTS audio to ffmpeg: {e}")))?;
    drop(stdin);

    let output = time::timeout(FFMPEG_CONVERSION_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            SpeechError::Request(format!(
                "ffmpeg timed out converting TTS audio to {} after {}s",
                target.to_openai(),
                FFMPEG_CONVERSION_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| SpeechError::Request(format!("failed to wait for ffmpeg conversion: {e}")))?;

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
        bytes: output.stdout,
        format: target,
        mime_type: target.mime_type().to_string(),
    })
}

fn ffmpeg_output_args(target: SpeechFormat) -> Vec<String> {
    match target {
        SpeechFormat::Mp3 => vec!["-f", "mp3"],
        SpeechFormat::Opus => vec!["-c:a", "libopus", "-f", "opus"],
        SpeechFormat::Aac => vec!["-c:a", "aac", "-f", "adts"],
        SpeechFormat::Flac => vec!["-f", "flac"],
        SpeechFormat::Wav => vec!["-f", "wav"],
        SpeechFormat::Pcm => vec!["-f", "s16le"],
    }
    .into_iter()
    .map(String::from)
    .collect()
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

    #[tokio::test]
    async fn wraps_pcm_as_wav() {
        let speech = SynthesizedSpeech {
            bytes: vec![0, 0, 1, 0, 255, 255, 0, 0],
            format: SpeechFormat::Pcm,
            mime_type: "audio/L16;codec=pcm;rate=24000".to_string(),
        };

        let converted = convert_speech(speech, SpeechFormat::Wav).await.unwrap();
        assert_eq!(converted.format, SpeechFormat::Wav);
        assert_eq!(converted.mime_type, "audio/wav");
        assert_eq!(&converted.bytes[..4], b"RIFF");
        assert_eq!(&converted.bytes[8..12], b"WAVE");
    }
}
