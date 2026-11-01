#!/usr/bin/env python3
"""Install the stdio memory adapter and register it in the current user's Codex config.

It registers the session credential (client.json) only; maintenance runs through agent-memory-client.
Claude Code's registration in ~/.claude.json runs the same installed file."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    source = Path(__file__).resolve().parent
    target = Path.home() / '.local/share/agent-memory-mcp'
    config_dir = Path.home() / '.config/agent-memory'
    if not (config_dir / 'client.json').is_file():
        parser.error(f'Missing {config_dir / "client.json"}; install the memory service/client credentials first')
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
    if codex_config.exists():  # one copy of the config before this run's change, replaced each run
        runtime = source.parent / 'runtime'
        runtime.mkdir(mode=0o700, exist_ok=True)
        backup = runtime / 'codex-config.toml'
        backup.unlink(missing_ok=True)
        with os.fdopen(os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as f:
            f.write(codex_config.read_bytes())
    # Do not silently replace an unrelated server with the same name.
    existing = subprocess.run(['codex', 'mcp', 'get', 'shared_memory', '--json'], capture_output=True, text=True)
    if existing.returncode == 0:
        entry = json.loads(existing.stdout)
        transport = entry.get('transport', entry)
        if str(executable) not in transport.get('args', []):
            parser.error('An unrelated shared_memory MCP server already exists; registration not changed')
    command = ['codex', 'mcp', 'add', 'shared_memory', '--', str(venv / 'bin/python'),
               str(executable), '--config', str(config_dir / 'client.json')]
    subprocess.run(command, check=True)
    print('Registered shared_memory. Reload MCP servers in the desktop client before using it.')


if __name__ == '__main__':
    main()
