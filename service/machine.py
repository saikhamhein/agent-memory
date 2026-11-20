#!/usr/bin/env python3
"""Add, list or revoke the token of another machine that uses this memory service.

A machine token has `recall` and `observe` only: the machine reads memories and suggests lessons. The dream and its
maintenance token stay on the machine that runs the service. `add` writes the machine's credentials to
~/.config/agent-memory/machine-NAME.json (mode 0600); copy that file to the other machine as
~/.config/agent-memory/client.json. The secret is never printed."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import secrets
import subprocess
import sys
import time
import tomllib
import urllib.parse

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('install_user', HERE / 'install-user.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)

RESERVED = {'agent-session', 'agent-recall', 'memory-maintainer'}
NAME = re.compile(r'[a-z0-9][a-z0-9-]{0,30}')
CAPABILITIES = ['recall', 'observe']


def load(server):
    config = tomllib.loads(server.read_text())
    unknown = sorted(set(config) - {'listen', 'database', 'tokens', 'projects'})
    if unknown:
        raise ValueError(f'server.toml has unknown keys {unknown}')
    return config


def save(server, config, tokens):
    installer.write_config({server: installer.render_server(config['listen'], config['database'], tokens)},
                           Path(config['database']).parent / 'runtime')
    subprocess.run([str(Path.home() / '.local/bin/agent-memory-server'), 'check', '--config', str(server)], check=True)


def restart(client):
    subprocess.run(['systemctl', '--user', 'restart', 'agent-memory'], check=True)
    for _ in range(50):
        if subprocess.run([client, 'status'], capture_output=True).returncode == 0:
            return
        time.sleep(0.1)
    raise RuntimeError('The service did not come back after the restart; run `systemctl --user status agent-memory`')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    add = commands.add_parser('add', help='Create a machine token and its credentials file')
    add.add_argument('name', help='Lowercase letters, digits and hyphens, for example laptop or pi')
    add.add_argument('--url', required=True, help='How that machine reaches the service, e.g. https://host.tailnet.ts.net:8787')
    remove = commands.add_parser('rm', help='Revoke a machine token')
    remove.add_argument('name')
    commands.add_parser('ls', help='List the machine tokens')
    args = parser.parse_args()
    config_dir = Path.home() / '.config/agent-memory'
    server = config_dir / 'server.toml'
    client = str(Path.home() / '.local/bin/agent-memory-client')
    try:
        config = load(server)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    tokens = config.get('tokens', [])
    machines = [t for t in tokens if t['name'] not in RESERVED]
    if args.command == 'ls':
        for token in machines:
            print(token['name'], ' '.join(token['capabilities']))
        return
    if args.command == 'rm':
        if args.name in RESERVED or args.name not in {t['name'] for t in machines}:
            parser.error(f'No machine token named {args.name}')
        save(server, config, [t for t in tokens if t['name'] != args.name])
        restart(client)
        (config_dir / f'machine-{args.name}.json').unlink(missing_ok=True)
        print(f'Revoked {args.name}. Its credentials stop working now.')
        return
    if not NAME.fullmatch(args.name) or args.name in RESERVED:
        parser.error('Name: lowercase letters, digits and hyphens, at most 31 characters, not a built-in token name')
    if args.name in {t['name'] for t in tokens}:
        parser.error(f'A token named {args.name} already exists')
    parsed = urllib.parse.urlparse(args.url)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        parser.error('--url must be an http(s) URL without credentials, query or fragment')
    secret = secrets.token_hex(32)
    token = {'name': args.name, 'sha256': installer.digest(secret), 'capabilities': CAPABILITIES}
    target = config_dir / f'machine-{args.name}.json'
    installer.private_file(target, installer.credentials(args.url, secret))
    try:
        save(server, config, [*tokens, token])
        restart(client)
    except BaseException:
        target.unlink(missing_ok=True)
        raise
    print(f'Added {args.name} (recall, observe). Copy {target} to that machine as ~/.config/agent-memory/client.json,')
    print('mode 0600, then delete the copy here.')


if __name__ == '__main__':
    try:
        main()
    except (subprocess.CalledProcessError, RuntimeError) as error:
        sys.exit(f'Error: {error}')
