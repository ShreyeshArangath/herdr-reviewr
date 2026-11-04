#!/usr/bin/env bash
# Build the checked-out fork so installation includes its local review workflow.
set -euo pipefail

NAME="herdr-reviewr"
SAY="reviewr"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT/bin"

cd "$ROOT"
cargo build --release --locked
mkdir -p "$BIN_DIR"
"$ROOT/scripts/swap-binary.sh" "$ROOT/target/release/$NAME" "$BIN_DIR/$NAME"
echo "$SAY: installed $BIN_DIR/$NAME"

# Stable launch paths as symlinks, re-pointed each install; a user's own file there is left alone.
# A staging checkout gets renamed, so every action re-points them at the live root too.
LINK_ROOT="${HERDR_PLUGIN_ROOT:-$ROOT}"
link_binary() {
  if mkdir -p "$1" 2>/dev/null && { [ -L "$1/$NAME" ] || [ ! -e "$1/$NAME" ]; } &&
    ln -sfn "$LINK_ROOT/bin/$NAME" "$1/$NAME" 2>/dev/null; then
    echo "$SAY: linked $1/$NAME"
  else
    echo "$SAY: warning: could not link $1/$NAME" >&2
  fi
}
link_binary "$HOME/.local/state/herdr/plugins/persiyanov.reviewr/bin"
if [ -d "$HOME/.local/bin" ]; then
  link_binary "$HOME/.local/bin"
fi
