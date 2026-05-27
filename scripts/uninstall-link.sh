#!/usr/bin/env bash
set -euo pipefail
TARGET="${HOME}/.local/bin/opencli"
if [ -L "$TARGET" ] || [ -f "$TARGET" ]; then
  rm -f "$TARGET"
  echo "✅ Removed $TARGET"
else
  echo "Nothing to remove at $TARGET"
fi
