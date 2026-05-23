use anyhow::{Context, Result};
use std::{
    io::Write,
    path::PathBuf,
    sync::{mpsc::SyncSender, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static LOG_SENDER: OnceLock<SyncSender<String>> = OnceLock::new();

pub fn init_tracing() -> Result<()> {
    let log_path = ensure_log_file()?;
    let file = open_log_append(&log_path)?;

    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1024);
    std::thread::spawn(move || {
        let mut file = file;
        while let Ok(line) = rx.recv() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    });

    LOG_SENDER
        .set(tx)
        .map_err(|_| anyhow::anyhow!("log file already initialized"))?;

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    tracing_subscriber::fmt().with_env_filter(filter).init();

    let _ = append_log_line(format!("logging initialized: {}", log_path.display()));
    Ok(())
}

pub fn log_file_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("codex-voice")
        .join("codex-voice.log")
}

pub fn append_log_line(message: impl AsRef<str>) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!("{timestamp} {}", message.as_ref());
    let sender = LOG_SENDER
        .get()
        .ok_or_else(|| anyhow::anyhow!("logging not initialized"))?;
    sender
        .try_send(line)
        .map_err(|_| anyhow::anyhow!("log channel full or closed"))?;
    Ok(())
}

pub fn ensure_log_file() -> Result<PathBuf> {
    let path = log_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }
    Ok(path)
}

pub fn open_log_append(path: &PathBuf) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))
}
