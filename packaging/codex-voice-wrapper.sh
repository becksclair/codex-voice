#!/bin/sh
# Wrapper script that sources user secrets before running codex-voice

SECRETS_FILE="$HOME/personal/dotfiles/secrets.sh"

if [ -f "$SECRETS_FILE" ]; then
	. "$SECRETS_FILE"
fi

exec /usr/local/bin/codex-voice "$@"
