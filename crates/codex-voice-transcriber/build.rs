use std::fs;
use std::path::{Path, PathBuf};

/// Stage the built web UI into `$OUT_DIR/web-dist` so `include_dir!` can embed
/// it. The real assets are produced by the Vite/React build in `web/dist`. When
/// that directory is absent (a fresh checkout that has not run the web build),
/// a minimal placeholder page is embedded instead so the crate always compiles.
fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let dest = out_dir.join("web-dist");
    let source = PathBuf::from("../../web/dist");
    let index = source.join("index.html");

    // Always rebuild when the presence of a built dist changes, so creating
    // `web/dist` later triggers a rebuild that swaps out the stub.
    println!("cargo:rerun-if-changed=../../web/dist");

    // Start from a clean staging directory to avoid leaving stale files behind
    // when switching between the real dist and the stub.
    if dest.exists() {
        fs::remove_dir_all(&dest).expect("clear staging web-dist directory");
    }
    fs::create_dir_all(&dest).expect("create staging web-dist directory");

    if index.exists() {
        copy_dir_recursive(&source, &dest);
        emit_rerun_for_tree(&source);
        println!("cargo:rustc-env=CODEX_VOICE_WEB_DIST_KIND=real");
    } else {
        fs::write(dest.join("index.html"), STUB_INDEX_HTML).expect("write stub index.html");
        println!("cargo:warning=web/dist not found; embedding stub web UI");
        println!("cargo:rustc-env=CODEX_VOICE_WEB_DIST_KIND=stub");
    }
}

const STUB_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Codex Voice</title>
</head>
<body>
<main>
<h1>Web UI not built</h1>
<p>Run <code>bun install &amp;&amp; bun run build</code> in <code>web/</code> to build the interface.</p>
</main>
</body>
</html>
"#;

fn copy_dir_recursive(source: &Path, dest: &Path) {
    for entry in fs::read_dir(source).expect("read web/dist directory") {
        let entry = entry.expect("read web/dist entry");
        let file_type = entry.file_type().expect("stat web/dist entry");
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir_all(&target).expect("create web-dist subdirectory");
            copy_dir_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy web-dist file");
        }
    }
}

fn emit_rerun_for_tree(source: &Path) {
    for entry in fs::read_dir(source).expect("read web/dist directory") {
        let entry = entry.expect("read web/dist entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("stat web/dist entry");
        if file_type.is_dir() {
            emit_rerun_for_tree(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
