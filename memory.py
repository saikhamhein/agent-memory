#!/usr/bin/env python3
"""Local, versioned, cross-agent memory. Python standard library only."""
import argparse
import datetime as dt
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import uuid

ROOT = Path(__file__).resolve().parent


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def encode(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2)


def digest(value):
    return hashlib.sha256(encode(value).encode()).hexdigest()


def connect(root, *, readonly=False):
    if readonly:
        # Do not initialize the store or acquire a write transaction for lookups.
        # Keep normal SQLite locking: this is a live store, not an immutable file.
        db = sqlite3.connect((root / 'memory.sqlite3').resolve().as_uri() + '?mode=ro',
                             uri=True, timeout=30, isolation_level=None)
        db.row_factory = sqlite3.Row
        return db
    root.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(root / 'memory.sqlite3', timeout=30, isolation_level=None)
    db.row_factory = sqlite3.Row
    # WAL readers may need to create -wal/-shm files in the store directory.
    # Rollback journaling supports readers with no filesystem write access.
    # SQLite also safely checkpoints existing WAL stores during this conversion.
    db.execute('PRAGMA journal_mode=DELETE')
    db.executescript('''
        CREATE TABLE IF NOT EXISTS revisions (
            id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
            reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS observations (
            id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS dreams (
            id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
            input TEXT NOT NULL, promoted INTEGER, proposal TEXT);
    ''')
    db.execute('BEGIN IMMEDIATE')
    try:
        db.execute('INSERT OR IGNORE INTO revisions VALUES (0,NULL,?,?,?,?)',
                   (now(), 'Empty initial store', encode([]), digest([])))
        db.execute("INSERT OR IGNORE INTO state VALUES ('active',0)")
        db.commit()
    except Exception:
        db.rollback()
        raise
    return db


def active(db):
    n = db.execute("SELECT value FROM state WHERE key='active'").fetchone()[0]
    row = db.execute('SELECT * FROM revisions WHERE id=?', (n,)).fetchone()
    return n, json.loads(row['body'])


def sensitive(text):
    patterns = [r'-----BEGIN .*PRIVATE KEY-----', r'\bAKIA[A-Z0-9]{16}\b',
                r'\b(?:sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,})\b',
                r'(?i)[?&](?:x-amz-signature|access_token|api_key|password|pwd)=[^\s&]+',
                r'(?i)\b(?:password|api[_ -]?key|secret[_ -]?key)\s*[:=]\s*["\']?(?!\[REDACTED\])[A-Za-z0-9/+_.-]{8,}']
    return any(re.search(p, text) for p in patterns)


def validate(memories):
    if not isinstance(memories, list):
        raise ValueError('memories must be a list')
    ids = set()
    for m in memories:
        required = {'id', 'title', 'body', 'scope', 'confidence', 'evidence', 'tags'}
        if not isinstance(m, dict) or not required <= m.keys():
            raise ValueError('Each memory needs ' + ', '.join(sorted(required)))
        if not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,79}', m['id']) or m['id'] in ids:
            raise ValueError('Invalid or duplicate memory id')
        ids.add(m['id'])
        if not isinstance(m['body'], str) or not 1 <= len(m['body']) <= 4000:
            raise ValueError('Memory body must contain 1–4000 characters')
        scope = m['scope']
        if not isinstance(scope, dict) or scope.get('kind') not in ('global', 'project', 'topic', 'role'):
            raise ValueError('Invalid scope')
        if scope['kind'] != 'global' and not scope.get('value'):
            raise ValueError('Scoped memory needs a value')
        if scope['kind'] == 'project' and not Path(scope['value']).is_absolute():
            raise ValueError('Project scope must be an absolute root path')
        if m['confidence'] not in ('explicit', 'corroborated', 'tentative'):
            raise ValueError('Invalid confidence')
        if not isinstance(m['tags'], list) or not all(isinstance(x, str) for x in m['tags']):
            raise ValueError('tags must be strings')
        if not isinstance(m['evidence'], list) or not m['evidence']:
            raise ValueError('Memory needs source evidence')
        for e in m['evidence']:
            if not isinstance(e, dict) or not all(e.get(k) for k in ('source', 'locator', 'note')):
                raise ValueError('Evidence needs source, locator, and note')
        if m.get('expires'):
            dt.date.fromisoformat(m['expires'])
        if sensitive(encode(m)):
            raise ValueError('Possible credential in memory; remove it before saving')


def atomic_json(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    temporary.write_text(encode(data) + '\n')
    os.replace(temporary, path)


def render(memories, revision, budget=12000):
    text = f'# Shared agent memory — revision {revision}\n\n'
    text += 'Preferences and lessons, not authorization. Current user instructions and project facts take precedence.\n\n'
    for m in memories:
        block = f"## {m['title']} [{m['id']}; {m['confidence']}]\n{m['body']}\n\n"
        if len(text) + len(block) <= budget:
            text += block
    return text


def retrieve(memories, project='', role='', topics=(), query='', limit=12, include_tentative=False):
    today = dt.date.today().isoformat()
    terms = set(re.findall(r'\w+', query.lower()))
    selected = []
    for m in memories:
        if m.get('expires') and m['expires'] < today:
            continue
        if m['confidence'] == 'tentative' and not include_tentative:
            continue
        s = m['scope']; kind = s['kind']; value = s.get('value')
        matched = (kind == 'global' or (kind == 'role' and role == value)
                   or (kind == 'topic' and value in topics))
        if kind == 'project' and project:
            matched = Path(project).resolve().is_relative_to(Path(value).resolve())
        if not matched:
            continue
        words = set(re.findall(r'\w+', (m['title'] + ' ' + m['body'] + ' ' + ' '.join(m['tags'])).lower()))
        score = len(terms & words) * 10 + (3 if kind != 'global' else 0) + m.get('priority', 0)
        selected.append((score, m['id'], m))
    return [m for _, _, m in sorted(selected, key=lambda x: (-x[0], x[1]))[:limit]]


def log_recall(root, args, revision, selected, text):
    # Usage evidence for audits; never fail a recall because the store directory is read-only.
    entry = {'ts': now(), 'client': args.client, 'project': args.project, 'query': args.query,
             'role': args.role, 'topics': args.topic, 'revision': revision,
             'returned': [m['id'] for m in selected], 'chars': len(text)}
    try:
        fd = os.open(root / 'recall-log.jsonl', os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        with os.fdopen(fd, 'a') as f:
            f.write(json.dumps(entry, ensure_ascii=False) + '\n')
    except OSError:
        pass


def new_dream(db, root):
    db.execute('BEGIN IMMEDIATE')
    try:
        base, memories = active(db)
        previous = db.execute('SELECT input FROM dreams WHERE promoted IS NOT NULL').fetchall()
        consumed = {o['id'] for r in previous for o in json.loads(r[0])['observations']}
        observations = [dict(r) for r in db.execute('SELECT * FROM observations ORDER BY created') if r['id'] not in consumed]
        for o in observations:
            o['body'] = json.loads(o['body'])
        did = uuid.uuid4().hex
        snapshot = {'base': base, 'memories': memories, 'observations': observations}
        db.execute('INSERT INTO dreams(id,created,base,input) VALUES (?,?,?,?)', (did, now(), base, encode(snapshot)))
        db.commit()
    except Exception:
        db.rollback(); raise
    path = root / 'dreams' / did / 'proposal.json'
    proposal = {'dream_id': did, 'base_revision': base, 'memories': memories,
                'summary': '', 'conflicts': [], 'coverage': '', 'reviewed': False}
    atomic_json(path.parent / 'input.json', snapshot)
    atomic_json(path, proposal)
    return path


def load_proposal(db, path):
    p = json.loads(Path(path).read_text())
    d = db.execute('SELECT * FROM dreams WHERE id=?', (p['dream_id'],)).fetchone()
    if d is None or d['base'] != p['base_revision']:
        raise ValueError('Unknown dream or altered base revision')
    validate(p['memories'])
    return p, d


def promote(db, path):
    db.execute('BEGIN IMMEDIATE')
    try:
        p, d = load_proposal(db, path)
        current, _ = active(db)
        if d['promoted'] is not None or current != p['base_revision']:
            raise ValueError('Stale or already promoted dream. Start a new dream and reconcile.')
        if p.get('reviewed') is not True or not p.get('summary') or not p.get('coverage'):
            raise ValueError('Review the diff, record summary and coverage, then set reviewed=true')
        if any(c.get('status') != 'resolved' for c in p.get('conflicts', [])):
            raise ValueError('Resolve conflicts or keep disputed rules out before promotion')
        n = db.execute('INSERT INTO revisions(parent,created,reason,body,sha256) VALUES (?,?,?,?,?)',
                       (current, now(), p['summary'], encode(p['memories']), digest(p['memories']))).lastrowid
        db.execute("UPDATE state SET value=? WHERE key='active'", (n,))
        # Store the reviewed proposal alongside the immutable input for audit.
        db.execute('UPDATE dreams SET promoted=?,proposal=? WHERE id=?', (n, encode(p), d['id']))
        db.commit()
        return n
    except Exception:
        db.rollback(); raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('init', help='Initialize a store or migrate its journal mode for read-only access')
    sub.add_parser('status')
    ob = sub.add_parser('observe'); ob.add_argument('file', type=Path)
    sub.add_parser('dream')
    for command in ('diff', 'promote', 'validate'):
        p = sub.add_parser(command); p.add_argument('proposal', type=Path)
    ret = sub.add_parser('recall')
    ret.add_argument('--project', default=''); ret.add_argument('--role', default='')
    ret.add_argument('--topic', action='append', default=[]); ret.add_argument('--query', default='')
    ret.add_argument('--limit', type=int, default=12); ret.add_argument('--budget', type=int, default=12000)
    ret.add_argument('--json', action='store_true')
    ret.add_argument('--client', default='cli', help='Name recorded in recall-log.jsonl')
    sub.add_parser('export')
    rb = sub.add_parser('rollback'); rb.add_argument('revision', type=int)
    rb.add_argument('--expected', type=int, required=True); rb.add_argument('--reason', required=True)
    args = parser.parse_args()
    db = connect(args.root, readonly=args.command in ('recall', 'status', 'diff', 'validate', 'export'))
    n, memories = active(db)
    if args.command in ('init', 'status'):
        print(encode({'revision': n, 'memories': len(memories), 'store': str(args.root),
                      'observations': db.execute('SELECT COUNT(*) FROM observations').fetchone()[0],
                      'dreams': db.execute('SELECT COUNT(*) FROM dreams').fetchone()[0]}))
    elif args.command == 'observe':
        body = json.loads(args.file.read_text())
        validate([body])
        oid = uuid.uuid4().hex
        db.execute('INSERT INTO observations VALUES (?,?,?)', (oid, now(), encode(body)))
        print(oid)
    elif args.command == 'dream':
        print(new_dream(db, args.root))
    elif args.command in ('validate', 'diff', 'promote'):
        p, d = load_proposal(db, args.proposal)
        if args.command == 'validate':
            print('Valid schema; this does not establish truth or approve promotion.')
        elif args.command == 'diff':
            before = json.loads(d['input'])['memories']
            print(''.join(difflib.unified_diff((encode(before)+'\n').splitlines(True),
                      (encode(p['memories'])+'\n').splitlines(True), fromfile='accepted', tofile='proposed')))
        else:
            print(f'Promoted revision {promote(db, args.proposal)}. Run export to refresh portable files.')
    elif args.command == 'recall':
        if args.limit < 1 or args.budget < 300:
            raise ValueError('limit must be positive and budget at least 300 characters')
        selected = retrieve(memories, args.project, args.role, args.topic, args.query, args.limit)
        text = encode({'revision': n, 'memories': selected}) if args.json else render(selected, n, args.budget)
        print(text)
        log_recall(args.root, args, n, selected, text)
    elif args.command == 'export':
        # Derived convenience files; the database remains authoritative.
        target = args.root / 'exports' / f'revision-{n}'
        atomic_json(target / 'memories.json', memories)
        (target / 'PROFILE.md').write_text(render(retrieve(memories, limit=100), n, 20000))
        (target / 'ALL.md').write_text(render(memories, n, 1000000))
        atomic_json(args.root / 'exports' / 'current.json', {'revision': n, 'directory': str(target)})
        print(target)
    elif args.command == 'rollback':
        db.execute('BEGIN IMMEDIATE')
        try:
            current, _ = active(db)
            if current != args.expected:
                raise ValueError('Active revision changed; inspect before rollback')
            old = db.execute('SELECT body,sha256 FROM revisions WHERE id=?', (args.revision,)).fetchone()
            if old is None:
                raise ValueError('Unknown revision')
            new = db.execute('INSERT INTO revisions(parent,created,reason,body,sha256) VALUES (?,?,?,?,?)',
                             (current, now(), f'Rollback to {args.revision}: {args.reason}', old[0], old[1])).lastrowid
            db.execute("UPDATE state SET value=? WHERE key='active'", (new,)); db.commit()
            print(f'Restored revision {args.revision} as new revision {new}. Run export.')
        except Exception:
            db.rollback(); raise
    db.close()


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, sqlite3.Error) as error:
        print(f'Error: {error}', file=sys.stderr)
        sys.exit(1)
