#!/bin/sh
# Bring this machine's memory setup up to date with origin/master. Run it in the
# main checkout after every merged PR.
# It never migrates: on a v1 store the installer refuses and this script stops.
# Migration is a one-time explicit installer step (design §3.6).
set -eu
cd "$(dirname "$0")"
[ ! -f .git ] || { echo "Run this in the main checkout, not a linked worktree" >&2; exit 1; }
[ "$(git branch --show-current)" = master ] || { echo "Run this in the master checkout" >&2; exit 1; }
before=$(git rev-parse HEAD)
git pull --ff-only origin master
# The pull may have replaced this file; the running shell would carry on with the old text.
[ "$(git rev-parse HEAD)" = "$before" ] || exec ./update-local.sh
cargo build --release --locked --manifest-path service/Cargo.toml
python3 service/install-user.py
mkdir -p "$HOME/.claude/hooks"
ln -sfn "$PWD/hooks/agent-memory-recall.py" "$HOME/.claude/hooks/agent-memory-recall.py"
echo "Updated to $(git rev-parse --short HEAD)."
