#!/usr/bin/env python3
"""Install this build as the current user's systemd service. No sudo required."""
import argparse
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


def migrate_database(binary, database):
    # Old Python stores use WAL. Migrate before a long-lived service connection
    # prevents the Python CLI from switching to rollback journaling.
    with sqlite3.connect(database.as_uri() + '?mode=ro', uri=True) as db:
        journal = db.execute('PRAGMA journal_mode').fetchone()[0]
    db.close()
    if journal == 'delete':
        return
    active = subprocess.run(['systemctl', '--user', 'is-active', '--quiet',
                             'agent-memory.service'], capture_output=True).returncode == 0
    if active:
        subprocess.run(['systemctl', '--user', 'stop', 'agent-memory.service'], check=True)
    try:
        subprocess.run([str(binary), 'init', '--database', str(database)], check=True)
    finally:
        # Restore the old service even if migration fails. The installer stops
        # on failure; on success its final restart picks up the new binary.
        if active:
            subprocess.run(['systemctl', '--user', 'start', 'agent-memory.service'], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database', type=Path, required=True, help='Existing authoritative SQLite database')
    args = parser.parse_args()
    database = args.database.resolve(strict=True)
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
    # Validate before replacing a running binary or its unit.
    subprocess.run([str(binary), 'check', '--config', str(server)], check=True)
    migrate_database(binary, database)
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
