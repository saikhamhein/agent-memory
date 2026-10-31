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
import tempfile
import time


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


def migrate_store(binary, database, pins):
    """Stop the service, back up, then migrate v1 to v2 (design §3.6).

    On failure the store is unchanged (migration is one transaction) and a
    service that was running is started again. On success it stays stopped
    until the caller restarts it with the new binary."""
    active = subprocess.run(['systemctl', '--user', 'is-active', '--quiet',
                             'agent-memory.service'], capture_output=True).returncode == 0
    if active:
        systemctl('stop')
    try:
        runtime = database.parent / 'runtime'
        runtime.mkdir(mode=0o700, exist_ok=True)
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
        backup = runtime / f'pre-v2-{stamp}.sqlite3'
        # `backup` refuses to overwrite and runs integrity_check on the copy.
        subprocess.run([str(binary), 'backup', '--database', str(database), '--output', str(backup)],
                       check=True)
        pin_args = [arg for pin in pins for arg in ('--pin', pin)]
        subprocess.run([str(binary), 'migrate', '--database', str(database), *pin_args], check=True)
    except BaseException:
        if active:
            systemctl('start')
        raise
    return backup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', type=Path, required=True, help='Existing authoritative SQLite database')
    parser.add_argument('--migrate', action='store_true',
                        help='Back up and migrate a v1 store to v2 (stops the service while migrating)')
    parser.add_argument('--pin', action='append', default=[], metavar='SLUG',
                        help='With --migrate: pin this memory (repeatable)')
    args = parser.parse_args()
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
    bin_dir = home / '.local/bin'
    config_dir = home / '.config/agent-memory'
    unit_dir = home / '.config/systemd/user'
    for directory in (bin_dir, config_dir, unit_dir):
        directory.mkdir(parents=True, exist_ok=True)
    config_dir.chmod(0o700)
    server = config_dir / 'server.toml'
    if not server.exists():
        if any((config_dir / name).exists() for name in ('client.json', 'maintainer.json')):
            parser.error('Client credentials already exist without a server config; reconcile them before installing')
        reader, maintainer = secrets.token_hex(32), secrets.token_hex(32)
        # JSON strings are valid TOML basic strings for ordinary Linux paths.
        text = ('listen = "127.0.0.1:8787"\n'
                f'database = {json.dumps(str(database), ensure_ascii=False)}\n\n'
                '[projects]\n'
                f'agent-memory = {json.dumps(str(source.parent), ensure_ascii=False)}\n\n'
                '[[tokens]]\nname = "agent-recall"\n'
                f'sha256 = "{hashlib.sha256(reader.encode()).hexdigest()}"\n'
                'capabilities = ["recall"]\n\n'
                '[[tokens]]\nname = "memory-maintainer"\n'
                f'sha256 = "{hashlib.sha256(maintainer.encode()).hexdigest()}"\n'
                'capabilities = ["recall", "observe", "review", "promote"]\n')
        private_file(server, text)
        for name, token in [('client.json', reader), ('maintainer.json', maintainer)]:
            private_file(config_dir / name, json.dumps({'url': 'http://127.0.0.1:8787', 'token': token}, indent=2) + '\n')
    else:
        import tomllib
        existing = tomllib.loads(server.read_text())
        if Path(existing['database']).resolve() != database:
            parser.error('Existing config points to a different database; no changes installed')
    if args.migrate:
        backup = migrate_store(binary, database, args.pin)
        print(f'Migrated to v2; backup at {backup}')
    # Validate before replacing a running binary or its unit.
    subprocess.run([str(binary), 'check', '--config', str(server)], check=True)
    atomic_copy(binary, bin_dir / 'agent-memory-server', 0o755)
    atomic_copy(source / 'client.py', bin_dir / 'agent-memory-client', 0o755)
    atomic_copy(source / 'deploy/agent-memory.user.service', unit_dir / 'agent-memory.service', 0o644)
    subprocess.run(['systemd-analyze', '--user', 'verify', str(unit_dir / 'agent-memory.service')], check=True)
    subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
    subprocess.run(['systemctl', '--user', 'enable', 'agent-memory.service'], check=True)
    subprocess.run(['systemctl', '--user', 'restart', 'agent-memory.service'], check=True)
    for _ in range(30):
        check = subprocess.run([str(bin_dir / 'agent-memory-client'), 'status'], capture_output=True, text=True)
        if check.returncode == 0:
            print(check.stdout, end='')
            break
        time.sleep(0.1)
    else:
        raise RuntimeError('Service did not become ready: ' + check.stderr)
    print('Installed and started agent-memory.service on http://127.0.0.1:8787')
    print('Credentials are in ~/.config/agent-memory (not printed).')


if __name__ == '__main__':
    main()
