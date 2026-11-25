"""catalog.py and the dream docs (design §4.3 C, triage X1, X3, X23-X28)."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

from service.tests.harness import REPO, SERVICE


class CatalogTests(unittest.TestCase):
    CATALOG = REPO / 'skill/dream-memory/scripts/catalog.py'

    def home_with_log(self, tmp):
        home = Path(tmp) / 'home'
        log = home / '.claude/projects/p/a.jsonl'
        log.parent.mkdir(parents=True)
        log.write_text('{}\n')
        return home

    def catalog(self, *arguments, **options):
        env = {key: value for key, value in os.environ.items() if key != 'XDG_STATE_HOME'}
        ran = subprocess.run([sys.executable, str(self.CATALOG), *arguments], check=True, capture_output=True,
                             text=True, timeout=30, env=env | options)
        return json.loads(ran.stdout)

    def test_writes_no_timestamped_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, root = self.home_with_log(tmp), Path(tmp) / 'root'
            for _ in range(2):
                self.catalog('--home', str(home), '--root', str(root))
            self.assertEqual(sorted(p.name for p in (root / 'sources').iterdir()),
                             ['changed-sessions.json', 'latest-catalog.json'])

    def test_writes_outside_the_checkout_by_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self.home_with_log(tmp)
            sessions = home / '.local/state/agent-memory/sources/changed-sessions.json'
            self.assertEqual(self.catalog('--home', str(home))['sessions'], str(sessions))
            self.assertTrue(sessions.is_file())

    def test_honours_xdg_state_home(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, state = self.home_with_log(tmp), Path(tmp) / 'state'
            sessions = state / 'agent-memory/sources/changed-sessions.json'
            self.assertEqual(self.catalog('--home', str(home), XDG_STATE_HOME=str(state))['sessions'], str(sessions))


class RemoteCatalogTests(CatalogTests):
    """A client machine's logs are pulled over ssh+rsync, then catalogued like local ones."""

    FAKE_SSH = """#!/bin/sh
# Stands in for ssh: drops the options and the host, then runs the command in the fake remote's home.
while [ $# -gt 0 ]; do
  case "$1" in -o|-l|-p|-i) shift 2;; -*) shift;; *) break;; esac
done
host="$1"; shift
[ "$host" = down ] && { echo "ssh: connect to host down: refused" >&2; exit 255; }
cd "$FAKE_REMOTE_HOME" && exec sh -c "$*"
"""
    # The inherited test methods would run twice; only the remote ones belong here.
    test_writes_no_timestamped_copy = test_writes_outside_the_checkout_by_default = test_honours_xdg_state_home = None

    def fake(self, tmp):
        ssh = Path(tmp) / 'bin/ssh'
        ssh.parent.mkdir(exist_ok=True)
        ssh.write_text(self.FAKE_SSH)
        ssh.chmod(0o755)
        remote = Path(tmp) / 'remote/home'
        if not remote.exists():
            self.home_with_log(Path(tmp) / 'remote')
        return dict(RSYNC_RSH=str(ssh), FAKE_REMOTE_HOME=str(remote))

    def test_pulls_a_clients_logs_and_catalogs_them(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, root = Path(tmp) / 'local', Path(tmp) / 'root'
            home.mkdir()
            first = self.catalog('--home', str(home), '--root', str(root), '--pull', 'laptop', **self.fake(tmp))
            sessions = json.loads((root / 'sources/changed-sessions.json').read_text())
            self.assertEqual([s['source'] for s in sessions], ['claude'])
            self.assertEqual(sessions[0]['locator'], str(root / 'remote/laptop/.claude/projects/p/a.jsonl'))
            self.assertTrue(Path(sessions[0]['locator']).is_file())
            self.assertEqual((first['files'], first['errors']), (1, 0))
            again = self.catalog('--home', str(home), '--root', str(root), '--pull', 'laptop', **self.fake(tmp))
            self.assertEqual(again['changed'], 0)

    def test_an_unreachable_client_is_reported_and_local_logs_still_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, root = self.home_with_log(Path(tmp) / 'local'), Path(tmp) / 'root'
            done = self.catalog('--home', str(home), '--root', str(root), '--pull', 'down', **self.fake(tmp))
            self.assertEqual((done['files'], done['changed']), (1, 1))
            self.assertGreaterEqual(done['errors'], 1)

    def test_hosts_come_from_the_hosts_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            home, root = Path(tmp) / 'local', Path(tmp) / 'root'
            (home / '.config/agent-memory').mkdir(parents=True)
            (home / '.config/agent-memory/dream-hosts').write_text('# clients\nuser@laptop\n\n')
            done = self.catalog('--home', str(home), '--root', str(root), **self.fake(tmp))
            self.assertEqual(done['files'], 1)
            self.assertTrue((root / 'remote/user_laptop/.claude/projects/p/a.jsonl').is_file())

    def test_a_host_that_looks_like_an_option_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            ran = subprocess.run([sys.executable, str(self.CATALOG), '--home', tmp, '--root', tmp, '--pull=-oProxyCommand=x'],
                                 capture_output=True, text=True, timeout=30)
            self.assertNotEqual(ran.returncode, 0)
            self.assertIn('not an ssh host', ran.stderr)


class DocsTests(unittest.TestCase):
    """The skill and the daily prompt may only name commands that exist (X3, X23)."""
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

    def test_only_real_client_commands_are_named(self):
        top = self.help_text()
        self.assertIn('dream', top)
        for doc in self.DOCS:
            text = doc.read_text()
            self.assertEqual(re.findall(r'\bmemory_[a-z][a-z_]*', text), [], f'{doc.name} names MCP tools')
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
