#!/usr/bin/env bash
set -euo pipefail
TARGET="${HOME}/.local/bin/tomte"
if [ -L "$TARGET" ] || [ -f "$TARGET" ]; then
  rm -f "$TARGET"
  echo "✅ Removed $TARGET"
else
  echo "Nothing to remove at $TARGET"
fi
