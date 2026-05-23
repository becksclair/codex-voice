use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` atomically with owner-only permissions.
///
/// On Unix this creates the file with mode `0o600` via `O_CREAT|O_EXCL` and
/// then sets restrictive permissions explicitly. On other platforms it falls
/// back to a plain `fs::write`.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        set_owner_only_file_permissions(path)?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
    }

    Ok(())
}

/// Atomically write `bytes` to `path` via a temporary file at `tmp_path`.
///
/// Writes to `tmp_path` with owner-only permissions, renames it to `path`,
/// then ensures `path` has owner-only permissions.  The caller must choose a
/// unique `tmp_path` (e.g. by including a random suffix).
pub fn write_private_file_atomic(
    path: &Path,
    tmp_path: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    write_private_file(tmp_path, bytes)?;
    std::fs::rename(tmp_path, path)?;
    // Permissions were already set to 0o600 by write_private_file on Unix.
    // rename(2) preserves those permissions, so no extra chmod is needed.
    Ok(())
}

/// Restrict directory permissions to owner-only (`0o700` on Unix).
pub fn set_owner_only_directory_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Restrict file permissions to owner-only (`0o600` on Unix).
pub fn set_owner_only_file_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_file_writes_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("secret.json");

        write_private_file(&path, b"secret").expect("private file write");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_private_file_roundtrip() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("secret.json");
        let tmp = dir.path().join("secret.json.tmp");

        write_private_file_atomic(&path, &tmp, b"secret").expect("atomic write");

        assert!(!tmp.exists());
        assert_eq!(std::fs::read(&path).expect("read"), b"secret");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
