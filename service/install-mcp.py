#!/usr/bin/env python3
"""Install the stdio memory adapter and register it in the current user's Codex config."""
import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--read-only', action='store_true', help='Expose only memory_recall and memory_status')
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    target = Path.home() / '.local/share/agent-memory-mcp'
    config_dir = Path.home() / '.config/agent-memory'
    for name in ['client.json'] + ([] if args.read_only else ['maintainer.json']):
        if not (config_dir / name).is_file():
            parser.error(f'Missing {config_dir / name}; install the memory service/client credentials first')
    target.mkdir(parents=True, exist_ok=True)
    target.chmod(0o700)
    venv = target / 'venv'
    if not (venv / 'bin/python').exists():
        subprocess.run(['uv', 'venv', str(venv)], check=True)
    subprocess.run(['uv', 'pip', 'sync', '--python', str(venv / 'bin/python'),
                    str(source / 'mcp-requirements.txt')], check=True)
    executable = target / 'mcp_server.py'
    with tempfile.NamedTemporaryFile(dir=target, delete=False) as f:
        staged = Path(f.name)
    try:
        shutil.copyfile(source / 'mcp_server.py', staged)
        staged.chmod(0o600)
        staged.replace(executable)
    finally:
        staged.unlink(missing_ok=True)
    codex_config = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'config.toml'
    if codex_config.exists():
        backup = source.parent / 'review' / ('mcp-config-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
        backup.mkdir(parents=True, mode=0o700)
        shutil.copyfile(codex_config, backup / 'config.toml')
        (backup / 'config.toml').chmod(0o600)
    # Do not silently replace an unrelated server with the same name.
    existing = subprocess.run(['codex', 'mcp', 'get', 'shared_memory', '--json'], capture_output=True, text=True)
    if existing.returncode == 0:
        entry = json.loads(existing.stdout)
        transport = entry.get('transport', entry)
        if str(executable) not in transport.get('args', []):
            parser.error('An unrelated shared_memory MCP server already exists; registration not changed')
    command = ['codex', 'mcp', 'add', 'shared_memory', '--', str(venv / 'bin/python'),
               str(executable), '--config', str(config_dir / 'client.json')]
    if not args.read_only:
        command += ['--maintenance-config', str(config_dir / 'maintainer.json')]
    subprocess.run(command, check=True)
    print('Registered shared_memory. Reload MCP servers in the desktop client before using it.')


if __name__ == '__main__':
    main()
