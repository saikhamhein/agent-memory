#!/usr/bin/env python3
"""Inventory local agent logs without copying their private contents into memory.

Writes sources/latest-catalog.json and sources/changed-sessions.json under the state directory: the new or changed
logs, newest first and at most 100, as the {source, locator, sha256} list that `agent-memory-client dream start
--sessions` takes. It prints where it wrote them.

Logs of the user's other machines are copied here first with rsync over ssh (`--pull HOST`, repeatable, or one ssh target
per line in ~/.config/agent-memory/dream-hosts) into <state>/remote/HOST, laid out like a home directory, and then
catalogued like local logs. A host that can't be reached is listed in `errors`; it doesn't stop the catalog."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import datetime as dt

MAX_SESSIONS = 100  # a dream takes at most 100 session locators
LOG_DIRECTORIES = ['.codex/sessions', '.codex/archived_sessions', '.claude/projects', '.dsh/sessions']


def pull(host, destination):
    """Copy a client's agent logs into destination, laid out like its home. Returns a list of errors."""
    rsh = os.environ.get('RSYNC_RSH') or 'ssh'
    errors = []
    for directory in LOG_DIRECTORIES:
        target = destination / directory
        target.mkdir(parents=True, exist_ok=True)
        try:
            ran = subprocess.run(['rsync', '-a', '-e', f'{rsh} -o BatchMode=yes -o ConnectTimeout=10',
                                  f'{host}:{directory}/', f'{target}/'], capture_output=True, text=True, timeout=900)
        except (OSError, subprocess.TimeoutExpired) as error:
            errors.append({'path': f'{host}:{directory}', 'error': str(error)})
            break
        # Exit 23 with "No such file" is a client that has no logs of that agent, which is normal.
        if ran.returncode and not (ran.returncode == 23 and 'No such file' in ran.stderr):
            errors.append({'path': f'{host}:{directory}', 'error': ran.stderr.strip()[-300:]})
            if ran.returncode in (255, 12, 30, 35):  # ssh or connection failure: the other directories would fail too
                break
        if not any(target.iterdir()):
            target.rmdir()
    return errors


def read_hosts(path):
    return [line.strip() for line in path.read_text().splitlines() if line.strip() and not line.startswith('#')] \
        if path.exists() else []


def scan(home):
    paths = []
    for provider, directory in [('codex', '.codex/sessions'), ('codex', '.codex/archived_sessions'),
                                ('claude', '.claude/projects')]:
        root = home / directory
        if root.exists():
            paths.extend((provider, p) for p in root.rglob('*.jsonl'))
    root = home / '.dsh/sessions'
    if root.exists():
        for directory in root.glob('*/*'):
            variants = sorted(directory.glob('session.v*.jsonl*'), key=lambda p: int(p.name.split('.')[1][1:]))
            if variants:
                paths.append(('deepseek', variants[-1]))
    return sorted(paths, key=lambda x: (x[0], str(x[1])))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--home', type=Path, default=Path.home())
    parser.add_argument('--root', type=Path, help='State directory; default $XDG_STATE_HOME/agent-memory')
    parser.add_argument('--pull', action='append', default=[], metavar='HOST',
                        help='ssh target of another machine whose logs to copy here first (repeatable)')
    args = parser.parse_args()
    hosts = args.pull or read_hosts(args.home / '.config/agent-memory/dream-hosts')
    for host in hosts:
        if host.startswith('-') or not re.fullmatch(r'[A-Za-z0-9._@:\[\]-]+', host):
            parser.error(f'not an ssh host: {host!r}')
    root = args.root or Path(os.environ.get('XDG_STATE_HOME') or args.home / '.local/state') / 'agent-memory'
    out = root / 'sources'
    out.mkdir(parents=True, exist_ok=True)
    previous_path = out / 'latest-catalog.json'
    previous = json.loads(previous_path.read_text()) if previous_path.exists() else {'files': []}
    old = {f['path']: f for f in previous['files']}
    result = {'created': dt.datetime.now(dt.timezone.utc).isoformat(), 'files': [], 'errors': [],
              'meaning': 'Inventory and change detection only; discovered is not semantically reviewed.'}
    homes = [args.home]
    for host in hosts:
        mirror = root / 'remote' / re.sub(r'[^A-Za-z0-9._-]', '_', host)
        result['errors'].extend(pull(host, mirror))
        homes.append(mirror)
    for provider, path in (found for home in homes for found in scan(home)):
        try:
            before = path.stat()
            sha = hashlib.sha256()
            with path.open('rb') as f:
                for block in iter(lambda: f.read(1024 * 1024), b''):
                    sha.update(block)
            after = path.stat()
            row = {'provider': provider, 'path': str(path), 'bytes': after.st_size,
                   'mtime_ns': after.st_mtime_ns, 'sha256': sha.hexdigest(),
                   'stable_during_scan': (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns)}
            row['changed_since_previous_scan'] = old.get(str(path), {}).get('sha256') != row['sha256']
            result['files'].append(row)
        except OSError as error:
            result['errors'].append({'path': str(path), 'error': str(error)})
    result['missing_since_previous_scan'] = sorted(set(old) - {f['path'] for f in result['files']})
    temporary = out / 'latest-catalog.json.tmp'
    temporary.write_text(json.dumps(result, indent=2) + '\n')
    temporary.replace(previous_path)
    changed = sorted((f for f in result['files'] if f['changed_since_previous_scan']), key=lambda f: -f['mtime_ns'])
    sessions = [{'source': f['provider'], 'locator': f['path'], 'sha256': f['sha256']} for f in changed[:MAX_SESSIONS]]
    (out / 'changed-sessions.json').write_text(json.dumps(sessions, indent=2) + '\n')
    print(json.dumps({'files': len(result['files']), 'changed': sum(f['changed_since_previous_scan'] for f in result['files']),
                      'errors': len(result['errors']), 'catalog': str(previous_path),
                      'sessions': str(out / 'changed-sessions.json'), 'sessions_left_out': max(0, len(changed) - MAX_SESSIONS)}))


if __name__ == '__main__':
    main()
