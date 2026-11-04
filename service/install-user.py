#!/usr/bin/env python3
"""Install this build as the current user's systemd service. No sudo required."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import tomllib

URL = 'http://127.0.0.1:8787'


def atomic_copy(source, target, mode):
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as temporary:
        path = Path(temporary.name)
    try:
        shutil.copyfile(source, path)
        path.chmod(mode)
        path.replace(target)
    finally:
        path.unlink(missing_ok=True)


def private_file(path, text):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as output:
        output.write(text)


def schema_version(database):
    with sqlite3.connect(database.as_uri() + '?mode=ro', uri=True) as db:
        version = db.execute('PRAGMA user_version').fetchone()[0]
    db.close()
    return version


def systemctl(*args):
    return subprocess.run(['systemctl', '--user', *args, 'agent-memory.service'], check=True)


def unit_is(state):
    return subprocess.run(['systemctl', '--user', state, '--quiet', 'agent-memory.service'],
                          capture_output=True).returncode == 0


STOPPED = ('The store is migrated to v2, but the install did not finish. agent-memory.service is stopped and '
           'disabled so the old binary cannot serve it. To recover, rerun install-user.py or roll back; '
           'the store is v2, so rerun without --migrate.')


def migrate_store(binary, database, pins):
    """Stop and disable the service, back up, then migrate v1 to v2 (design §3.6).

    The outcome is read from the store's user_version, not the exit code: before the commit the store is
    unchanged (migration is one transaction) and the old service is put back as it was. After the commit the
    service stays stopped and disabled, so the v1 binary never serves a v2 store; only main() enables it,
    once the new binary and unit are installed."""
    active, enabled = unit_is('is-active'), unit_is('is-enabled')
    if active:
        systemctl('stop')
    if enabled:
        systemctl('disable')
    try:
        runtime = database.parent / 'runtime'
        runtime.mkdir(mode=0o700, exist_ok=True)
        backup = runtime / f'pre-v2-{stamp()}.sqlite3'
        # `backup` refuses to overwrite and runs integrity_check on the copy.
        subprocess.run([str(binary), 'backup', '--database', str(database), '--output', str(backup)],
                       check=True)
        pin_args = [arg for pin in pins for arg in ('--pin', pin)]
        subprocess.run([str(binary), 'migrate', '--database', str(database), *pin_args], check=True)
    except BaseException:
        if schema_version(database) >= 2:
            print(STOPPED, file=sys.stderr)
            raise
        if enabled:
            systemctl('enable')
        if active:
            systemctl('start')
        raise
    return backup


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')


def toml_string(value):
    # JSON strings are valid TOML basic strings for ordinary Linux paths and names.
    return json.dumps(str(value), ensure_ascii=False)


def render_server(listen, database, tokens):
    text = f'listen = {toml_string(listen)}\ndatabase = {toml_string(database)}\n'
    for token in tokens:
        text += (f'\n[[tokens]]\nname = {toml_string(token["name"])}\nsha256 = {toml_string(token["sha256"])}\n'
                 f'capabilities = {json.dumps(token["capabilities"])}\n')
    return text


def credentials(url, token):
    return json.dumps({'url': url, 'token': token}, indent=2) + '\n'


def digest(token):
    return hashlib.sha256(token.encode()).hexdigest()


def reconcile_tokens(server_text, client_text):
    """Token reconcile (design §3.6): `agent-session` = [recall, observe] in client.json, no `agent-recall`,
    no `[projects]`; every other token is kept as it is. Returns the new (server.toml, client.json) texts."""
    config = tomllib.loads(server_text)
    unknown = sorted(set(config) - {'listen', 'database', 'tokens', 'projects'})
    if unknown:
        raise ValueError(f'server.toml has unknown keys {unknown}; remove them and rerun')
    client = json.loads(client_text) if client_text else {}
    secret = client.get('token', '')
    tokens = config.get('tokens', [])
    if not any(t['name'] == 'agent-session' and t['sha256'] == digest(secret) for t in tokens):
        secret = secrets.token_hex(32)
    session = {'name': 'agent-session', 'sha256': digest(secret), 'capabilities': ['recall', 'observe']}
    kept = [t for t in tokens if t['name'] not in ('agent-session', 'agent-recall')]
    return (render_server(config['listen'], config['database'], [session, *kept]),
            credentials(client.get('url', URL), secret))


def write_config(files, runtime):
    """Back up each file that changes to runtime/, then replace it atomically. Unchanged files are left alone."""
    suffix = stamp()
    for path, text in files.items():
        if path.exists():
            if path.read_text() == text:
                continue
            runtime.mkdir(mode=0o700, exist_ok=True)
            private_file(runtime / f'{path.name}.{suffix}', path.read_text())
        with tempfile.NamedTemporaryFile('w', dir=path.parent, delete=False) as temporary:  # created 0600
            temporary.write(text)
        Path(temporary.name).replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', type=Path,
                        help='Existing authoritative SQLite database; required on a first install, then read from server.toml')
    parser.add_argument('--migrate', action='store_true',
                        help='Back up and migrate a v1 store to v2 (stops the service while migrating)')
    parser.add_argument('--pin', action='append', default=[], metavar='SLUG',
                        help='With --migrate: pin this memory (repeatable)')
    args = parser.parse_args()
    server = Path.home() / '.config/agent-memory/server.toml'
    if args.database is None:
        if not server.exists():
            parser.error('First install: pass --database')
        args.database = Path(tomllib.loads(server.read_text())['database'])
    database = args.database.resolve(strict=True)
    version = schema_version(database)
    if version < 2 and not args.migrate:
        parser.error('This is a v1 store; nothing was changed. Rerun with --migrate (it backs up first)')
    if version >= 2 and args.migrate:
        parser.error('The store is already v2; rerun without --migrate')
    if args.pin and not args.migrate:
        parser.error('--pin needs --migrate')
    source = Path(__file__).resolve().parent
    binary = source / 'target/release/agent-memory-server'
    if not binary.is_file():
        parser.error('Build first: cargo build --release --locked --manifest-path service/Cargo.toml')
    home = Path.home()
    checkout = source.parent
    if (checkout / '.git').is_file():
        parser.error('Run this in the main checkout, not a linked worktree')
    links = [home / parent / 'skills/dream-memory' for parent in ('.claude', '.agents')]
    for link in links:
        if link.exists() and not link.is_symlink():
            parser.error(f'{link} is not a link; move it aside and rerun')
    bin_dir = home / '.local/bin'
    config_dir = home / '.config/agent-memory'
    unit_dir = home / '.config/systemd/user'
    for directory in (bin_dir, config_dir, unit_dir):
        directory.mkdir(parents=True, exist_ok=True)
    config_dir.chmod(0o700)
    client = config_dir / 'client.json'
    files = {}
    if server.exists():
        server_text = server.read_text()
        if Path(tomllib.loads(server_text)['database']).resolve() != database:
            parser.error('Existing config points to a different database; no changes installed')
    else:
        if any((config_dir / name).exists() for name in ('client.json', 'maintainer.json')):
            parser.error('Client credentials already exist without a server config; reconcile them before installing')
        maintainer = secrets.token_hex(32)
        files[config_dir / 'maintainer.json'] = credentials(URL, maintainer)
        server_text = render_server('127.0.0.1:8787', database, [{
            'name': 'memory-maintainer', 'sha256': digest(maintainer),
            'capabilities': ['recall', 'observe', 'review', 'promote']}])
    try:
        files[server], files[client] = reconcile_tokens(server_text, client.read_text() if client.exists() else None)
    except ValueError as error:
        parser.error(str(error))
    if args.migrate:
        backup = migrate_store(binary, database, args.pin)
        print(f'Migrated to v2; backup at {backup}')
    try:
        install(binary, source, server, files, database.parent / 'runtime', bin_dir, unit_dir)
    except BaseException:
        if args.migrate:
            print(STOPPED, file=sys.stderr)
        raise
    start(bin_dir)
    link_skill(links, checkout / 'skill/dream-memory')
    carry_catalog(checkout, home)
    print('Installed and started agent-memory.service on http://127.0.0.1:8787')
    print('Credentials are in ~/.config/agent-memory (not printed).')


def link_skill(links, skill):
    for link in links:
        link.parent.mkdir(parents=True, exist_ok=True)
        link.unlink(missing_ok=True)
        link.symlink_to(skill)


def carry_catalog(checkout, home):
    # The catalog used to live in the checkout; without it the next dream takes every log for new.
    old = checkout / 'sources/latest-catalog.json'
    state = Path(os.environ.get('XDG_STATE_HOME') or home / '.local/state') / 'agent-memory/sources'
    if old.exists() and not (state / old.name).exists():
        state.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(old, state / old.name)


def install(binary, source, server, files, runtime, bin_dir, unit_dir):
    write_config(files, runtime)
    # Validate before replacing a running binary or its unit.
    subprocess.run([str(binary), 'check', '--config', str(server)], check=True)
    atomic_copy(binary, bin_dir / 'agent-memory-server', 0o755)
    atomic_copy(source / 'client.py', bin_dir / 'agent-memory-client', 0o755)
    atomic_copy(source / 'deploy/agent-memory.user.service', unit_dir / 'agent-memory.service', 0o644)
    subprocess.run(['systemd-analyze', '--user', 'verify', str(unit_dir / 'agent-memory.service')], check=True)
    subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)


def start(bin_dir):
    # Enabling comes only now, after the binary and unit that can serve a v2 store are installed.
    systemctl('enable')
    systemctl('restart')
    for _ in range(30):
        check = subprocess.run([str(bin_dir / 'agent-memory-client'), 'status'], capture_output=True, text=True)
        if check.returncode == 0:
            print(check.stdout, end='')
            break
        time.sleep(0.1)
    else:
        raise RuntimeError('Service did not become ready: ' + check.stderr)


if __name__ == '__main__':
    main()
