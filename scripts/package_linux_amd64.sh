#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

source_commit=${1:-}
output_dir=${2:-output/releases}

if [[ ! $source_commit =~ ^[0-9a-f]{40}$ ]]; then
    echo "usage: $0 <lowercase-full-commit-sha> [output-directory]" >&2
    exit 2
fi
if [[ $(git rev-parse HEAD) != "$source_commit" ]]; then
    echo "source commit does not match the checked-out HEAD" >&2
    exit 1
fi
if [[ -n $(git status --porcelain --untracked-files=all -- . ':(exclude)output') ]]; then
    echo "refusing to package a dirty source tree" >&2
    exit 1
fi
if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
    echo "linux-amd64 artifacts must be built on Linux x86_64" >&2
    exit 1
fi

archive_name="codex-voice-linux-amd64-${source_commit}.tar.gz"
archive="$output_dir/$archive_name"
sidecar="${archive}.sha256"
if [[ -e $archive || -e $sidecar ]]; then
    echo "refusing to overwrite existing artifact output: $archive" >&2
    exit 1
fi

(cd web && bun install --frozen-lockfile && bun run build)
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
    cargo build --locked --release -p codex-voice-app --bin codex-voice

temp_root=$(mktemp -d "${TMPDIR:-/tmp}/codex-voice-package.XXXXXX")
trap 'rm -rf -- "$temp_root"' EXIT
stage="$temp_root/codex-voice"
install -d -m 0755 "$stage"
install -m 0755 target/release/codex-voice "$stage/codex-voice"
python3 - "$source_commit" "$stage/artifact.json" <<'PY'
import json
import pathlib
import sys

source_commit, destination = sys.argv[1:]
manifest = {
    "executable": "codex-voice",
    "health_path": "/healthz",
    "platform": "linux-amd64",
    "schema_version": "saga-binary-service-artifact/v1",
    "service_id": "codex-voice",
    "source_commit": source_commit,
}
pathlib.Path(destination).write_text(
    json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
chmod 0644 "$stage/artifact.json"

mkdir -p "$output_dir"
partial_archive="$temp_root/$archive_name"
tar \
    --sort=name \
    --mtime='@0' \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --format=gnu \
    -C "$temp_root" \
    -cf - codex-voice \
    | gzip -n > "$partial_archive"
mv "$partial_archive" "$archive"
sha256sum "$archive" | sed "s#  .*#  $archive_name#" > "$sidecar"

python3 scripts/validate_runtime_artifact.py "$archive" "$source_commit" --smoke
printf '%s\n' "$archive" "$sidecar"
