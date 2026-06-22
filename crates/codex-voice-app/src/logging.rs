use anyhow::{Context, Result};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{mpsc::SyncSender, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static LOG_SENDER: OnceLock<SyncSender<String>> = OnceLock::new();
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const ROTATED_LOG_FILES: usize = 5;
const LOCK_RETRY_AFTER: Duration = Duration::from_millis(10);
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const STALE_LOCK_AFTER: Duration = Duration::from_secs(30);

pub fn init_tracing() -> Result<()> {
    let log_path = ensure_log_file()?;
    let writer = RotatingLogWriter::open(log_path.clone())?;

    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(1024);
    std::thread::spawn(move || {
        let mut writer = writer;
        while let Ok(line) = rx.recv() {
            let _ = writer.write_line(&line);
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

pub fn open_log_append(path: &Path) -> Result<std::fs::File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))
}

struct RotatingLogWriter {
    path: PathBuf,
}

impl RotatingLogWriter {
    fn open(path: PathBuf) -> Result<Self> {
        let _lock = LogFileLock::acquire(lock_log_path(&path))?;
        rotate_log_if_needed(&path)?;
        Ok(Self { path })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        let mut payload = Vec::with_capacity(line.len() + 1);
        payload.extend_from_slice(line.as_bytes());
        payload.push(b'\n');

        let _lock = LogFileLock::acquire(lock_log_path(&self.path))?;
        rotate_log_if_needed_for_write(&self.path, payload.len() as u64)?;
        let mut file = open_log_append(&self.path)?;
        file.write_all(&payload)?;
        file.flush()?;
        Ok(())
    }
}

struct LogFileLock {
    path: PathBuf,
}

impl LogFileLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        let started = Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_stale_lock(&path)?;
                    if started.elapsed() >= LOCK_TIMEOUT {
                        anyhow::bail!("timed out acquiring log file lock {}", path.display());
                    }
                    thread::sleep(LOCK_RETRY_AFTER);
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to acquire log file lock {}", path.display())
                    });
                }
            }
        }
    }
}

impl Drop for LogFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn rotate_log_if_needed(path: &Path) -> Result<()> {
    let current_len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat log file {}", path.display()));
        }
    };

    if current_len >= MAX_LOG_BYTES {
        rotate_log_files(path)?;
    }
    Ok(())
}

fn rotate_log_if_needed_for_write(path: &Path, incoming_bytes: u64) -> Result<()> {
    let current_len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to stat log file {}", path.display()));
        }
    };

    if current_len > 0 && current_len.saturating_add(incoming_bytes) > MAX_LOG_BYTES {
        rotate_log_files(path)?;
    }
    Ok(())
}

fn rotate_log_files(path: &Path) -> Result<()> {
    let oldest = rotated_log_path(path, ROTATED_LOG_FILES);
    match std::fs::remove_file(&oldest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to remove old log file {}", oldest.display()));
        }
    }

    for index in (1..ROTATED_LOG_FILES).rev() {
        let from = rotated_log_path(path, index);
        let to = rotated_log_path(path, index + 1);
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to rotate log file {} to {}",
                        from.display(),
                        to.display()
                    )
                });
            }
        }
    }

    match std::fs::rename(path, rotated_log_path(path, 1)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to rotate {}", path.display())),
    }
}

fn rotated_log_path(path: &Path, index: usize) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(format!(".{index}"));
    PathBuf::from(path)
}

fn lock_log_path(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn remove_stale_lock(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    let Ok(age) = modified.elapsed() else {
        return Ok(());
    };
    if age > STALE_LOCK_AFTER {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove stale log lock {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        lock_log_path, rotate_log_files, rotated_log_path, LogFileLock, RotatingLogWriter,
    };

    fn temp_log_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "codex-voice-log-rotation-test-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ))
            .join("codex-voice.log")
    }

    #[test]
    fn rotates_existing_log_files() {
        let path = temp_log_path("chain");
        let parent = path.parent().expect("temp path has parent");
        std::fs::create_dir_all(parent).expect("create temp log dir");
        std::fs::write(&path, b"current").expect("write current log");
        std::fs::write(rotated_log_path(&path, 1), b"previous").expect("write rotated log");

        rotate_log_files(&path).expect("rotate logs");

        assert!(!path.exists());
        assert_eq!(
            std::fs::read(rotated_log_path(&path, 1)).expect("read first rotated log"),
            b"current"
        );
        assert_eq!(
            std::fs::read(rotated_log_path(&path, 2)).expect("read second rotated log"),
            b"previous"
        );

        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn writer_reopens_active_log_after_external_rotation() {
        let path = temp_log_path("reopen");
        let parent = path.parent().expect("temp path has parent");
        std::fs::create_dir_all(parent).expect("create temp log dir");
        let mut writer = RotatingLogWriter::open(path.clone()).expect("open writer");
        std::fs::write(&path, b"current\n").expect("write current log");

        rotate_log_files(&path).expect("rotate logs");
        writer.write_line("after rotation").expect("write log line");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read active log"),
            "after rotation\n"
        );
        assert_eq!(
            std::fs::read(rotated_log_path(&path, 1)).expect("read first rotated log"),
            b"current\n"
        );

        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn log_file_lock_is_exclusive() {
        let path = temp_log_path("lock");
        let parent = path.parent().expect("temp path has parent");
        std::fs::create_dir_all(parent).expect("create temp log dir");
        let lock_path = lock_log_path(&path);
        let lock = LogFileLock::acquire(lock_path.clone()).expect("acquire lock");

        assert!(LogFileLock::acquire(lock_path.clone()).is_err());
        drop(lock);
        assert!(LogFileLock::acquire(lock_path).is_ok());

        let _ = std::fs::remove_dir_all(parent);
    }
}
