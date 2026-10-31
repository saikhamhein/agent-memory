#!/usr/bin/env python3
"""Claude Code SessionStart hook: inject the shared-memory start view once per session.

~/.claude/hooks/agent-memory-recall.py links here; the client is service/client.py beside it. If the
service is down or hangs, it injects the read-only mirror's INDEX.md (found through the server's config),
cut to fit Claude Code's 10,000-character cap. Each run appends one line to
~/.cache/agent-memory/recall-log.jsonl, so missed injections show up. It always exits 0."""
import json
from pathlib import Path
import subprocess
import sys
import time
import tomllib

CLIENT = Path(__file__).resolve().parents[1] / 'service/client.py'
CONFIG = Path.home() / '.config/agent-memory'
LOG = Path.home() / '.cache/agent-memory/recall-log.jsonl'
START_MAX = 9500  # recall.rs START_MAX: Claude Code shows only a preview above 10,000 characters


def canonical_root(cwd):
    """The main checkout for a worktree or the repository itself; otherwise cwd."""
    try:
        common = subprocess.run(['git', '-C', cwd, 'rev-parse', '--path-format=absolute', '--git-common-dir'],
                                capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return cwd
    return common[:-len('/.git')] if common.endswith('/.git') else cwd


def from_service(project):
    try:
        recalled = subprocess.run([sys.executable, str(CLIENT), 'recall', '--project', project,
                                   '--client', 'session-start'], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return ''
    return recalled.stdout if recalled.returncode == 0 else ''


def from_mirror():
    try:
        config = tomllib.loads((CONFIG / 'server.toml').read_text())
        index = Path(config.get('mount') or Path(config['database']).with_name('mnt')) / 'shared-agent-memory/INDEX.md'
        text = 'The memory service is unavailable; this is the mirror index.\n\n' + index.read_text()
    except (OSError, ValueError, KeyError, TypeError):
        return ''
    if len(text) <= START_MAX:
        return text
    tail = f'… truncated; full index: {index}\n'
    return text[:text.rfind('\n', 0, START_MAX - len(tail)) + 1] + tail


try:
    event = json.load(sys.stdin)
    event = event if isinstance(event, dict) else {}
except ValueError:
    event = {}
source = event.get('source', '')
project = canonical_root(event.get('cwd') or str(Path.cwd()))
if source not in ('resume', 'fork'):  # a resumed or forked transcript already holds the start view
    text, via = from_service(project), 'service'
    if not text:
        text, via = from_mirror(), 'index'
    if not text:
        via = 'none'
    try:
        LOG.parent.mkdir(parents=True, exist_ok=True)
        with LOG.open('a') as log:
            log.write(json.dumps({'ts': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'session_id': event.get('session_id', ''),
                                  'source': source, 'project': project, 'chars': len(text), 'ok': bool(text),
                                  'via': via}) + '\n')
    except OSError:
        pass
    if text:
        print(json.dumps({'hookSpecificOutput': {'hookEventName': 'SessionStart', 'additionalContext': text}}))
