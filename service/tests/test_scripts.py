"""install-mcp.py, catalog.py and the dream docs (design §4.3 C, triage X1, X3, X23-X28).

install-mcp.py runs against the real `claude` and `codex` CLIs, but with HOME and CODEX_HOME in a
temporary directory and a fake `uv`, so nothing outside the temporary directory is read or written.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

from service.tests.harness import REPO, SERVICE

SESSION_TOOLS = {'memory_status', 'memory_recall', 'memory_list', 'memory_read', 'memory_observe', 'memory_useful'}
FAKE_UV = '''#!/bin/sh
echo "$@" >> "$UV_LOG"
if [ "$1" = venv ]; then mkdir -p "$2/bin"; ln -sf "$REAL_PYTHON" "$2/bin/python"; fi
'''


@unittest.skipUnless(shutil.which('claude') and shutil.which('codex'), 'needs the claude and codex CLIs')
class InstallMcpTests(unittest.TestCase):
    CLAUDE_JSON, CODEX_TOML = Path.home() / '.claude.json', Path.home() / '.codex/config.toml'

    @classmethod
    def digests(cls):
        """What install-mcp.py could change: Claude's user-scope `mcpServers` (running sessions rewrite the rest
        of ~/.claude.json all the time) and the whole Codex config."""
        try:
            servers = json.loads(cls.CLAUDE_JSON.read_text()).get('mcpServers')
        except (OSError, ValueError):
            servers = None
        codex = hashlib.sha256(cls.CODEX_TOML.read_bytes()).hexdigest() if cls.CODEX_TOML.is_file() else None
        return json.dumps(servers, sort_keys=True), codex

    def setUp(self):
        # The real CLIs run here, so prove they never touch the user's own configuration.
        before = self.digests()
        self.addCleanup(lambda: self.assertEqual(before, self.digests(),
                                                 'the test changed the real ~/.claude.json mcpServers or ~/.codex/config.toml'))
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name).resolve()
        self.repo, self.home, self.codex = root / 'repo', root / 'home', root / 'codex'
        (self.repo / 'service').mkdir(parents=True)
        for name in ('install-mcp.py', 'mcp_server.py', 'mcp-requirements.txt'):
            shutil.copy(SERVICE / name, self.repo / 'service' / name)
        self.config = self.home / '.config/agent-memory'
        self.config.mkdir(parents=True)
        (self.config / 'client.json').write_text('{"url": "http://127.0.0.1:1", "token": "t"}')
        self.codex.mkdir()
        (self.codex / 'config.toml').write_text('model = "fixture"\n')
        bin_dir = root / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'uv').write_text(FAKE_UV)
        (bin_dir / 'uv').chmod(0o755)
        self.env = {'PATH': f'{bin_dir}:{os.environ["PATH"]}', 'HOME': str(self.home), 'CODEX_HOME': str(self.codex),
                    'UV_LOG': str(root / 'uv.log'), 'REAL_PYTHON': sys.executable}
        self.executable = self.home / '.local/share/agent-memory-mcp/mcp_server.py'
        self.client_json = self.config / 'client.json'

    def run_tool(self, *args, check=True):
        result = subprocess.run(args, capture_output=True, text=True, timeout=60, env=self.env, cwd=self.home)
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def install(self, *args, check=True):
        return self.run_tool(sys.executable, str(self.repo / 'service/install-mcp.py'), *args, check=check)

    def codex_entry(self):
        return json.loads(self.run_tool('codex', 'mcp', 'get', 'shared_memory', '--json').stdout)['transport']

    def claude_entry(self):
        return self.run_tool('claude', 'mcp', 'get', 'shared_memory', check=False).stdout

    def test_registers_the_session_credential_only_in_both_clients(self):
        (self.config / 'maintainer.json').write_text('{"url": "http://127.0.0.1:1", "token": "m"}')
        self.install()
        entry = self.codex_entry()
        self.assertEqual(entry['args'], [str(self.executable), '--config', str(self.client_json)])
        self.assertEqual(entry['command'], str(self.home / '.local/share/agent-memory-mcp/venv/bin/python'))
        claude = self.claude_entry()
        self.assertIn(f'Args: {self.executable} --config {self.client_json}', claude)
        self.assertNotIn('maintain', claude + json.dumps(entry))
        self.assertEqual(self.executable.read_bytes(), (SERVICE / 'mcp_server.py').read_bytes())
        uv = (self.repo.parent / 'uv.log').read_text()
        self.assertIn('pip sync', uv)

    def test_maintainer_json_is_not_needed(self):
        self.assertFalse((self.config / 'maintainer.json').exists())
        self.install()
        self.install()  # idempotent: the second run leaves the same registrations
        self.assertEqual(self.codex_entry()['args'][-1], str(self.client_json))
        self.assertEqual(self.claude_entry().count('shared_memory:'), 1)

    def test_old_flags_are_gone(self):
        for flag in ('--read-only', '--maintenance-config'):
            result = self.install(flag, check=False)
            self.assertEqual(result.returncode, 2, flag)
        self.assertNotIn('Added', self.claude_entry())

    def test_one_fixed_backup_of_the_codex_config_under_runtime(self):
        self.install()
        backup = self.repo / 'runtime' / 'codex-config.toml'
        self.assertEqual(backup.read_text(), 'model = "fixture"\n')
        self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o600)
        self.assertFalse((self.repo / 'review').exists())
        (self.codex / 'config.toml').write_text('model = "changed"\n')
        self.install()
        self.assertEqual(sorted(p.name for p in (self.repo / 'runtime').iterdir()), ['codex-config.toml'])
        self.assertIn('"changed"', backup.read_text())  # overwritten, not accumulated

    def test_a_claude_entry_with_the_maintenance_credential_is_replaced(self):
        python = self.home / '.local/share/agent-memory-mcp/venv/bin/python'
        self.run_tool('claude', 'mcp', 'add', '--scope', 'user', 'shared_memory', '--', str(python), str(self.executable),
                      '--config', str(self.client_json), '--maintenance-config', str(self.config / 'maintainer.json'))
        self.assertIn('--maintenance-config', self.claude_entry())
        self.install()
        entry = self.claude_entry()
        self.assertNotIn('maintain', entry)
        self.assertIn(f'--config {self.client_json}', entry)

    def test_an_unrelated_claude_entry_is_refused_and_kept(self):
        self.run_tool('claude', 'mcp', 'add', '--scope', 'user', 'shared_memory', '--', '/usr/bin/true')
        result = self.install(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('Missing', result.stderr)  # refused for the entry, not for a missing credential file
        self.assertIn('Command: /usr/bin/true', self.claude_entry())


class CatalogTests(unittest.TestCase):
    def test_writes_no_timestamped_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, root = Path(tmp) / 'home', Path(tmp) / 'root'
            log = home / '.claude/projects/p/a.jsonl'
            log.parent.mkdir(parents=True)
            log.write_text('{}\n')
            for _ in range(2):
                subprocess.run([sys.executable, str(REPO / 'catalog.py'), '--home', str(home), '--root', str(root)],
                               check=True, capture_output=True, timeout=30)
            self.assertEqual(sorted(p.name for p in (root / 'sources').iterdir()),
                             ['changed-sessions.json', 'latest-catalog.json'])


class DocsTests(unittest.TestCase):
    """The skill and the daily prompt may only name tools and commands that exist (X3, X23)."""
    DOCS = [REPO / 'skill/dream-memory/SKILL.md', REPO / 'ops/daily-memory-dream.md']

    def help_text(self, *command):
        return subprocess.run([sys.executable, str(SERVICE / 'client.py'), *command, '-h'],
                              capture_output=True, text=True, timeout=20).stdout

    def commands(self, text):
        """Client command lines written in backticks or code blocks: [(subcommand words, flags)]."""
        spans = re.findall(r'`([^`\n]+)`', text) + re.findall(r'```.*?\n(.*?)```', text, re.S)
        found = []
        for span in spans:
            for line in span.splitlines():
                if 'agent-memory-client' not in line:
                    continue
                words = [w.strip('`.,;:()"\'') for w in line.split('agent-memory-client', 1)[1].split()]
                words = [w for w in words if w and w != '…']
                flags = [w for w in words if w.startswith('--') and w != '--config']
                positional = []
                skip = False
                for w in words:
                    if skip:
                        skip = False
                    elif w == '--config':
                        skip = True
                    elif not w.startswith('-'):
                        positional.append(w)
                if positional:
                    found.append((positional, flags))
        return found

    def test_only_session_tools_and_real_client_commands_are_named(self):
        top = self.help_text()
        self.assertIn('dream', top)
        for doc in self.DOCS:
            text = doc.read_text()
            named = set(re.findall(r'\bmemory_[a-z][a-z_]*', text))
            self.assertEqual(named - SESSION_TOOLS, set(), f'{doc.name} names tools the adapter no longer has')
            for positional, flags in self.commands(text):
                sub = positional[:2] if positional[0] == 'dream' and len(positional) > 1 else positional[:1]
                self.assertIn(sub[0], top, f'{doc.name}: unknown client command {sub}')
                if len(sub) == 2:
                    self.assertIn(sub[1], self.help_text('dream'), f'{doc.name}: unknown dream command {sub}')
                for flag in flags:
                    self.assertIn(flag, self.help_text(*sub), f'{doc.name}: {sub} has no {flag}')

    def test_the_dream_cycle_matches_the_contract(self):
        text = '\n'.join(doc.read_text() for doc in self.DOCS)
        self.assertTrue('dream_running' in text, 'docs must say what to do on a dream_running 409 (X10)')
        self.assertTrue('hour' in text, 'docs must give the stale-dream rule: cancel one older than an hour (X10)')
        self.assertNotIn('observations_consumed', text)  # X10 removed it
        self.assertIsNone(re.search(r'\breviewed\s*[=:]|`reviewed`', text), 'the adopt body has no reviewed flag (X14)')
        self.assertIn('maintainer.json', text)  # dreams run through the client CLI with the maintenance credential
        self.assertIn('4,000', text)  # the pinned budget the report states


if __name__ == '__main__':
    unittest.main()
