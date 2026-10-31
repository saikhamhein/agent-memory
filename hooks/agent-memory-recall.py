#!/usr/bin/env python3
"""Claude Code SessionStart hook: inject the shared-memory start view once per session.

~/.claude/hooks/agent-memory-recall.py links here; the client is service/client.py beside it. If the
service is down it injects the read-only mirror's INDEX.md, found through the server's config.
It fails open: on any other problem it prints nothing and exits 0."""
import json
from pathlib import Path
import subprocess
import sys
import tomllib

CLIENT = Path(__file__).resolve().parents[1] / 'service/client.py'
SERVER_CONFIG = Path.home() / '.config/agent-memory/server.toml'


def canonical_root(cwd):
    """The main checkout for a worktree or the repository itself; otherwise cwd."""
    try:
        common = subprocess.run(['git', '-C', cwd, 'rev-parse', '--path-format=absolute', '--git-common-dir'],
                                capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return cwd
    return common[:-len('/.git')] if common.endswith('/.git') else cwd


def context(event):
    if event.get('source') in ('resume', 'fork'):
        return ''  # The transcript already holds the start view.
    project = canonical_root(event.get('cwd') or str(Path.cwd()))
    try:
        recalled = subprocess.run([sys.executable, str(CLIENT), 'recall',
                                   '--project', project, '--client', 'session-start'],
                                  capture_output=True, text=True, timeout=20)
        if recalled.returncode == 0 and recalled.stdout.strip():
            return recalled.stdout
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        config = tomllib.loads(SERVER_CONFIG.read_text())
        mount = Path(config.get('mount') or Path(config['database']).with_name('mnt'))
        index = (mount / 'shared-agent-memory/INDEX.md').read_text()
    except (OSError, ValueError, KeyError, TypeError):
        return ''
    return 'The memory service is unavailable; this is the mirror index.\n\n' + index


try:
    event = json.load(sys.stdin)
except ValueError:
    event = {}
text = context(event if isinstance(event, dict) else {})
if text:
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'SessionStart', 'additionalContext': text}}))
