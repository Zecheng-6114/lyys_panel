#!/bin/sh
# Install versioned git hooks from scripts/git-hooks/ into .git/hooks/.
# Run once after every fresh `git init` or .git restore.
set -e
ROOT="$(git rev-parse --show-toplevel)"
SRC="$ROOT/scripts/git-hooks"
DST="$ROOT/.git/hooks"
chmod +x "$SRC"/*
for h in "$SRC"/*; do
  cp "$h" "$DST/$(basename "$h")"
  echo "installed hook: $(basename "$h")"
done
