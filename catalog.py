#!/usr/bin/env python3
"""Inventory local agent logs without copying their private contents into memory.

Writes sources/latest-catalog.json and sources/changed-sessions.json: the new or changed logs, newest first
and at most 100, as the {source, locator, sha256} list that `agent-memory-client dream start --sessions` takes."""
import argparse
import hashlib
import json
from pathlib import Path
import datetime as dt

MAX_SESSIONS = 100  # a dream takes at most 100 session locators


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
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parent)
    args = parser.parse_args()
    out = args.root / 'sources'
    out.mkdir(parents=True, exist_ok=True)
    previous_path = out / 'latest-catalog.json'
    previous = json.loads(previous_path.read_text()) if previous_path.exists() else {'files': []}
    old = {f['path']: f for f in previous['files']}
    result = {'created': dt.datetime.now(dt.timezone.utc).isoformat(), 'files': [], 'errors': [],
              'meaning': 'Inventory and change detection only; discovered is not semantically reviewed.'}
    for provider, path in scan(args.home):
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
