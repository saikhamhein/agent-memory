#!/bin/sh
# Bring this machine's memory setup up to date with origin/master. Run it in the
# main checkout after every merged PR.
# It never migrates: on a v1 store the installer refuses and this script stops.
# Migration is a one-time explicit installer step (design §3.6).
set -eu
cd "$(dirname "$0")"
[ "$(git branch --show-current)" = master ] || { echo "Run this in the master checkout" >&2; exit 1; }
git pull --ff-only origin master
cargo build --release --locked --manifest-path service/Cargo.toml
python3 service/install-user.py
python3 service/install-mcp.py
mkdir -p "$HOME/.claude/hooks"
ln -sfn "$PWD/hooks/agent-memory-recall.py" "$HOME/.claude/hooks/agent-memory-recall.py"
echo "Updated to $(git rev-parse --short HEAD). Restart agent sessions to load the new MCP adapter."
