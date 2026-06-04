#!/usr/bin/env bash
# install-link.sh — Link tomte into $HOME/.local/bin so it auto-syncs when
# you rebuild.
#
# After running this script, every time you edit code and run
# `cargo build --release`, the binary at $HOME/.local/bin/tomte updates
# automatically (because it is a symlink).
#
# Two modes:
#   --release  (default) — symlink to target/release/tomte; you must run
#                          `cargo build --release` after editing.
#   --dev                — install a wrapper script that runs `cargo run`
#                          on every invocation (slower, but no manual rebuild).
#
# Usage:
#   ./scripts/install-link.sh           # release symlink
#   ./scripts/install-link.sh --dev     # auto-rebuild wrapper

set -euo pipefail

MODE="release"
case "${1:-}" in
  --dev) MODE="dev" ;;
  --release|"") MODE="release" ;;
  *) echo "Usage: $0 [--release|--dev]"; exit 1 ;;
esac

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
TARGET="${BIN_DIR}/tomte"

mkdir -p "${BIN_DIR}"

# Make sure $BIN_DIR is on PATH
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "⚠  $BIN_DIR is not on PATH. Add this to ~/.bashrc / ~/.zshrc:"
     echo "     export PATH=\"\$HOME/.local/bin:\$PATH\""
     ;;
esac

if [ "$MODE" = "release" ]; then
  echo "→ Building release…"
  (cd "$REPO" && cargo build --release --bin tomte)
  ln -sf "$REPO/target/release/tomte" "$TARGET"
  echo "✅ Linked: $TARGET → $REPO/target/release/tomte"
  echo "   From now on each \`cargo build --release\` updates the binary."
else
  cat >"$TARGET" <<EOF
#!/usr/bin/env bash
# Auto-rebuild wrapper for tomte. Runs \`cargo run\` on every invocation.
exec cargo run --quiet --manifest-path "$REPO/Cargo.toml" --bin tomte -- "\$@"
EOF
  chmod +x "$TARGET"
  echo "✅ Installed dev wrapper: $TARGET"
  echo "   Every \`tomte\` invocation will rebuild on demand if the source has changed."
fi

echo
echo "Try it now:  tomte --help"
