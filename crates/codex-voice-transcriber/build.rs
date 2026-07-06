use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=src/server.rs");
    println!("cargo:rerun-if-changed=assets/web/icon-192.png");
    println!("cargo:rerun-if-changed=assets/web/icon-512.png");
    println!("cargo:rerun-if-changed=assets/web/icon-maskable-512.png");
    println!("cargo:rerun-if-changed=assets/web/apple-touch-icon.png");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    if let Some(head_ref) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        println!("cargo:rerun-if-changed=../../.git/{head_ref}");
    }

    let revision = build_revision();
    println!("cargo:rustc-env=CODEX_VOICE_WEB_REVISION={revision}");
}

fn build_revision() -> String {
    let timestamp = unix_timestamp();
    let Some(commit) = git_output(&["rev-parse", "--short=12", "HEAD"]) else {
        return sanitize_revision(&format!("nogit-{timestamp}"));
    };

    if git_dirty() {
        sanitize_revision(&format!("{commit}-dirty-{timestamp}"))
    } else {
        sanitize_revision(&commit)
    }
}

fn git_dirty() -> bool {
    !git_status_ok(&["diff", "--quiet", "--ignore-submodules", "--"])
        || !git_status_ok(&["diff", "--cached", "--quiet", "--ignore-submodules", "--"])
}

fn git_status_ok(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn sanitize_revision(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
