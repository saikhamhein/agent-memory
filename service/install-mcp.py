#!/usr/bin/env python3
"""Install the stdio memory adapter and register it for Codex and, at user scope, Claude Code.

It registers the session credential (client.json) only; maintenance runs through agent-memory-client.
A matching registration is left alone, an older one of ours is replaced, and an unrelated one is refused."""
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
    python, wanted = str(venv / 'bin/python'), [str(executable), '--config', str(config_dir / 'client.json')]
    # Leave a matching entry alone, replace our own older one, and never replace an unrelated server.
    existing = subprocess.run(['codex', 'mcp', 'get', 'shared_memory', '--json'], capture_output=True, text=True)
    if existing.returncode == 0:
        entry = json.loads(existing.stdout)
        transport = entry.get('transport', entry)
        if str(executable) not in transport.get('args', []):
            parser.error('An unrelated shared_memory MCP server already exists in Codex; registration not changed')
    if existing.returncode != 0 or [transport.get('command'), *transport.get('args', [])] != [python, *wanted]:
        subprocess.run(['codex', 'mcp', 'add', 'shared_memory', '--', python, *wanted], check=True)
    if shutil.which('claude'):  # user scope, through the CLI: ~/.claude.json is never edited here
        existing = subprocess.run(['claude', 'mcp', 'get', 'shared_memory'], capture_output=True, text=True)
        fields = dict(line.strip().split(': ', 1) for line in existing.stdout.splitlines() if ': ' in line)
        ours = fields.get('Scope', '').startswith('User') and str(executable) in fields.get('Args', '')
        if existing.returncode == 0 and not ours:
            parser.error('Claude Code has a shared_memory MCP server this installer did not add; registration not changed')
        if existing.returncode != 0 or (fields.get('Command'), fields.get('Args')) != (python, ' '.join(wanted)):
            if existing.returncode == 0:
                subprocess.run(['claude', 'mcp', 'remove', 'shared_memory', '--scope', 'user'], check=True)
            subprocess.run(['claude', 'mcp', 'add', '--scope', 'user', 'shared_memory', '--', python, *wanted],
                           check=True)
    print('Registered shared_memory. Reload MCP servers in the desktop client before using it.')


if __name__ == '__main__':
    main()
