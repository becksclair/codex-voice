use clap::{Args, Parser, Subcommand};
use std::net::SocketAddr;

/// CLI entry-point types and argument-to-config conversions.

#[derive(Debug, Parser)]
#[command(
    name = "codex-voice",
    version,
    about = "Hold-to-dictate desktop utility backed by local Codex auth"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run,
    Server(ServerArgs),
    Doctor {
        #[command(subcommand)]
        command: Option<DoctorCommand>,
    },
    Transcriber {
        #[command(subcommand)]
        command: TranscriberCommand,
    },
    Tts {
        #[command(subcommand)]
        command: TtsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum TtsCommand {
    /// Benchmark speech-prep models against a fixed sample.
    Bench(super::tts::TtsBenchArgs),
}

#[derive(Debug, Subcommand)]
pub enum DoctorCommand {
    Audio(AudioDoctor),
    CodexAuth,
    Transcribe(TranscribeDoctor),
    Tts(super::tts::TtsDoctor),
    Hotkey,
    Paste(PasteDoctor),
    LinuxPortals,
}

#[derive(Debug, Args)]
pub struct AudioDoctor {
    #[arg(long, default_value_t = 2)]
    pub seconds: u64,
    #[arg(long)]
    pub keep: bool,
}

#[derive(Debug, Args)]
pub struct TranscribeDoctor {
    #[arg(long)]
    pub file: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub struct PasteDoctor {
    #[arg(long)]
    pub text: String,
}

#[derive(Debug, Subcommand)]
pub enum TranscriberCommand {
    ProbeLimits(TranscriberProbeLimitsArgs),
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    #[arg(long, default_value = "127.0.0.1:3845")]
    pub bind: SocketAddr,
    #[arg(long, default_value_t = 24)]
    pub codex_upload_limit_mib: u64,
    #[arg(long, default_value_t = 512)]
    pub client_upload_limit_mib: u64,
    #[arg(long, default_value_t = 600)]
    pub chunk_seconds: u64,
    #[arg(long, default_value = "CODEX_VOICE_TRANSCRIBER_TOKEN")]
    pub token_env: String,
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg_binary: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Require bearer token authentication"
    )]
    pub require_auth: bool,
    #[arg(
        long,
        help = "Serve the web UI from this directory instead of the embedded build"
    )]
    pub web_dist: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct TranscriberProbeLimitsArgs {
    #[arg(long)]
    pub file: std::path::PathBuf,
    #[arg(long, default_value_t = 24)]
    pub codex_upload_limit_mib: u64,
    #[arg(long, default_value_t = 600)]
    pub chunk_seconds: u64,
    #[arg(long, default_value_t = 1)]
    pub max_chunks: usize,
    #[arg(long)]
    pub include_oversized: bool,
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg_binary: String,
}

impl TryFrom<ServerArgs> for codex_voice_transcriber::ServeConfig {
    type Error = anyhow::Error;

    fn try_from(value: ServerArgs) -> anyhow::Result<Self> {
        let ip = value.bind.ip();
        if !ip.is_loopback() && !is_tailscale_ip(&ip) {
            anyhow::bail!(
                "server must bind to a loopback or Tailscale address (e.g. 127.0.0.1 or 100.x.x.x); {} is not allowed",
                ip
            );
        }
        Ok(Self {
            bind: value.bind,
            codex_upload_limit_bytes: mib_to_bytes(value.codex_upload_limit_mib)?,
            client_upload_limit_bytes: mib_to_bytes(value.client_upload_limit_mib)?,
            chunk_seconds: value.chunk_seconds,
            token_env: value.token_env,
            ffmpeg_binary: value.ffmpeg_binary,
            no_auth: !value.require_auth,
            web_dist_override: value.web_dist,
        })
    }
}

impl TryFrom<TranscriberProbeLimitsArgs> for codex_voice_transcriber::ProbeLimitsConfig {
    type Error = anyhow::Error;

    fn try_from(value: TranscriberProbeLimitsArgs) -> anyhow::Result<Self> {
        Ok(Self {
            file: value.file,
            codex_upload_limit_bytes: mib_to_bytes(value.codex_upload_limit_mib)?,
            chunk_seconds: value.chunk_seconds,
            max_chunks: value.max_chunks,
            include_oversized: value.include_oversized,
            ffmpeg_binary: value.ffmpeg_binary,
        })
    }
}

/// Check whether an IP address is in a Tailscale range.
///
/// Tailscale assigns IPv4 CGNAT addresses (100.64.0.0/10) and IPv6 ULA
/// addresses (fd7a:115c:a1e0::/48).
fn is_tailscale_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            // 100.64.0.0/10 => first octet 100, second octet 64-127
            octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            // fd7a:115c:a1e0::/48 => first three segments must match
            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

fn mib_to_bytes(mib: u64) -> anyhow::Result<u64> {
    use anyhow::Context;
    let bytes = mib
        .checked_mul(1024 * 1024)
        .context("MiB value is too large")?;
    anyhow::ensure!(bytes > 0, "MiB value must be greater than zero");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_args(bind: SocketAddr) -> ServerArgs {
        ServerArgs {
            bind,
            codex_upload_limit_mib: 24,
            client_upload_limit_mib: 512,
            chunk_seconds: 600,
            token_env: "CODEX_VOICE_TRANSCRIBER_TOKEN".to_string(),
            ffmpeg_binary: "ffmpeg".to_string(),
            require_auth: false,
            web_dist: None,
        }
    }

    #[test]
    fn server_config_rejects_non_loopback_non_tailscale_bind() {
        let bind = "0.0.0.0:3845".parse().expect("socket addr");

        let error = codex_voice_transcriber::ServeConfig::try_from(server_args(bind))
            .expect_err("non-loopback non-tailscale bind should fail");

        assert!(error
            .to_string()
            .contains("server must bind to a loopback or Tailscale address"));
    }

    #[test]
    fn server_config_accepts_loopback_bind() {
        let bind = "127.0.0.1:3845".parse().expect("socket addr");

        let config = codex_voice_transcriber::ServeConfig::try_from(server_args(bind))
            .expect("loopback bind should succeed");

        assert_eq!(config.bind, bind);
    }

    #[test]
    fn server_config_accepts_tailscale_bind() {
        let bind = "100.120.202.119:3845".parse().expect("socket addr");

        let config = codex_voice_transcriber::ServeConfig::try_from(server_args(bind))
            .expect("tailscale bind should succeed");

        assert_eq!(config.bind, bind);
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("cli should parse")
    }

    #[test]
    fn parses_tts_bench_defaults() {
        let cli = parse(&["codex-voice", "tts", "bench"]);
        match cli.command {
            Some(Command::Tts {
                command: TtsCommand::Bench(args),
            }) => {
                assert!(args.text.is_none());
                assert!(args.file.is_none());
                assert!(args.models.is_none());
                assert_eq!(args.iterations, 1);
                assert!(!args.dry_run);
            }
            other => panic!("expected tts bench, got {other:?}"),
        }
    }

    #[test]
    fn parses_tts_bench_flags() {
        let cli = parse(&[
            "codex-voice",
            "tts",
            "bench",
            "--dry-run",
            "--iterations",
            "3",
            "--models",
            "gpt-5.5-none,gemini-3.5-flash",
        ]);
        match cli.command {
            Some(Command::Tts {
                command: TtsCommand::Bench(args),
            }) => {
                assert!(args.dry_run);
                assert_eq!(args.iterations, 3);
                assert_eq!(
                    args.models,
                    Some(vec![
                        "gpt-5.5-none".to_string(),
                        "gemini-3.5-flash".to_string(),
                    ])
                );
            }
            other => panic!("expected tts bench, got {other:?}"),
        }
    }

    #[test]
    fn tts_bench_rejects_text_and_file_together() {
        let error = Cli::try_parse_from([
            "codex-voice",
            "tts",
            "bench",
            "--text",
            "hi",
            "--file",
            "/tmp/x.txt",
        ])
        .expect_err("--text and --file must conflict");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn server_config_accepts_tailscale_ipv6_bind() {
        let bind = "[fd7a:115c:a1e0:ab12:cd34:ef56:7890:1234]:3845"
            .parse()
            .expect("socket addr");

        let config = codex_voice_transcriber::ServeConfig::try_from(server_args(bind))
            .expect("tailscale IPv6 bind should succeed");

        assert_eq!(config.bind, bind);
    }
}
