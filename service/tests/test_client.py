"""agent-memory-client and the SessionStart hook against the real server (design §4.3 C), on a scratch store.

Standard library only, so these also run without the MCP virtual environment.
"""
import json
import os
import socket
import shutil
import subprocess
import sys
import time
import unittest

from service.tests.harness import REPO, content, sha, start_server

HOOK = REPO / 'hooks' / 'agent-memory-recall.py'
START_HEADING = '# Shared agent memory — revision'
# Non-canonical JSON spacing and Burmese text: the store keeps the bytes as given.
ODD = ('---\n{ "title" : "မြန်မာ note","confidence":"explicit",  "tags":["မြန်မာ"],'
       '"evidence":[{"source":"t","locator":"l","note":"n"}] }\n---\nမင်္ဂလာပါ\n\n  trailing  \n')


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.server = start_server(self)
        self.files = self.server.root / 'files'
        self.files.mkdir()

    def file(self, name, text):
        path = self.files / name
        path.write_bytes(text.encode())
        return path

    def run_client(self, *args, role='admin', ok=True):
        result = self.server.client(*args, role=role)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertEqual(result.stdout, '')
        return result

    def json(self, *args, **kwargs):
        return json.loads(self.run_client(*args, **kwargs).stdout)

    def test_memory_roundtrip(self):
        path, other = '/global/note.md', '/topics/hardware/note.md'
        odd = self.file('odd.md', ODD)
        created = self.json('write', path, odd)
        self.assertEqual((created['path'], created['content_sha256']), (path, sha(ODD)))
        self.assertEqual([m['path'] for m in self.json('ls')['data']], [path])
        self.assertEqual(self.json('ls', '/topics')['data'], [])
        stdout = self.server.client('cat', path, binary=True).stdout
        self.assertEqual(stdout, ODD.encode())  # exact bytes, no trailing newline added
        self.assertIn('path_exists', self.run_client('write', path, odd, ok=False).stderr)
        new = self.file('new.md', content('Note', 'Second text.'))
        stale = self.run_client('write', path, new, '--expected', sha('other'), ok=False)
        self.assertIn('precondition_failed', stale.stderr)
        self.assertEqual(self.server.client('cat', path, binary=True).stdout, ODD.encode())
        updated = self.json('write', path, new, '--expected', sha(ODD))
        self.assertEqual(updated['content_sha256'], sha(content('Note', 'Second text.')))
        renamed = self.json('write', path, new, '--expected', updated['content_sha256'], '--to', other)
        self.assertEqual((renamed['id'], renamed['path']), (created['id'], other))
        self.assertEqual(self.run_client('cat', path, ok=False).stdout, '')
        history = self.json('versions', other)['data']
        self.assertEqual([v['operation'] for v in history], ['modified', 'modified', 'created'])
        head, _, oldest = history
        self.assertIn('HTTP 400', self.run_client('redact', head['id'], ok=False).stderr)
        redacted = self.json('redact', oldest['id'])
        self.assertIsNotNone(redacted['redacted_at'])
        self.assertIsNone(redacted.get('content'))
        wrong = self.run_client('rm', other, '--expected', sha('x'), ok=False)
        self.assertIn('precondition_failed', wrong.stderr)
        self.run_client('rm', other, '--expected', renamed['content_sha256'])
        self.assertEqual(self.json('ls')['data'], [])
        self.assertEqual(self.json('versions')['data'][0]['operation'], 'deleted')

    def test_reader_can_read_but_not_write(self):
        self.server.seed('/global/keep.md', 'Keep', pinned=True)
        note = self.file('n.md', content('N'))
        self.assertEqual(self.json('status', role='reader')['revision'], 1)
        self.assertEqual(self.json('ls', role='reader')['data'][0]['path'], '/global/keep.md')
        self.assertIn('Check existing evidence', self.run_client('cat', '/global/keep.md', role='reader').stdout)
        self.assertIn('403', self.run_client('write', '/global/n.md', note, role='reader', ok=False).stderr)
        self.assertIn('403', self.run_client('rm', '/global/keep.md', role='reader', ok=False).stderr)
        self.assertIn('403', self.run_client('observe', '/global/n.md', note, role='reader', ok=False).stderr)
        self.assertIn('403', self.run_client('dream', 'start', role='reader', ok=False).stderr)
        self.assertEqual(self.json('status')['memories'], 1)

    def test_dream_workflow(self):
        self.server.seed('/global/existing.md', 'Existing', 'Original body.')
        self.run_client('observe', '/global/candidate.md', self.file('c.md', content('Candidate')), role='observer')
        sessions = self.file('sessions.json', json.dumps([{'source': 'claude-code', 'locator': '/x/a.jsonl'}]))
        started = self.json('dream', 'start', '--sessions', sessions)
        self.assertEqual((started['status'], len(started['observations']), len(started['sessions'])), ('running', 1, 1))
        output = started['output_store_id']
        added = self.file('z.md', content('Zebrafish husbandry', 'Zebrafish tanks need weekly water changes.'))
        self.run_client('write', '/global/zebrafish.md', added, '--store', output)
        shown = self.json('dream', 'show', started['id'], '--diff')
        self.assertEqual([m['path'] for m in shown['diff']['added']], ['/global/zebrafish.md'])
        self.assertIn(started['id'], [d['id'] for d in self.json('dream', 'show')['data']])
        review = self.file('review.json', json.dumps(dict(summary='Add zebrafish', coverage='client test',
                                                          conflicts=[], reviewed=True)))
        self.assertEqual(self.json('dream', 'adopt', started['id'], review)['status'], 'completed')
        self.assertIn('Zebrafish tanks need weekly water changes.', self.run_client('recall', '--query', 'zebrafish husbandry').stdout)
        self.run_client('dream', 'adopt', started['id'], review, ok=False)  # only a running dream can be adopted
        second = self.json('dream', 'start')
        self.assertEqual(second['observations'], [])
        self.assertEqual(self.json('dream', 'cancel', second['id'])['status'], 'canceled')
        self.assertEqual(self.json('dream', 'show', second['id'])['status'], 'canceled')

    def test_recall_prints_the_start_view_and_ignores_old_flags(self):
        self.server.seed('/global/keep.md', 'Keep it simple', 'Remove what is not needed.', pinned=True)
        plain = self.run_client('recall').stdout
        self.assertTrue(plain.startswith(START_HEADING))
        self.assertIn('Remove what is not needed.', plain)
        old = self.run_client('recall', '--limit', '12', '--budget', '12000', '--json').stdout
        self.assertEqual(old, plain)
        with_options = self.run_client('recall', '--project', '/home/sai/x', '--role', 'coordinator',
                                       '--topic', 'hardware', '--client', 'session-start')
        self.assertIn('Remove what is not needed.', with_options.stdout)
        entry = self.server.recall_entries()[-1]
        self.assertEqual((entry['client'], entry['view'], entry['project'], entry['role'], entry['topics']),
                         ('session-start', 'start', '/home/sai/x', 'coordinator', ['hardware']))
        nothing = self.run_client('recall', '--query', 'kubernetes ingress yaml')
        self.assertEqual(nothing.stdout, 'No memory matches "kubernetes ingress yaml".\n')
        self.assertEqual(self.server.recall_entries()[-1]['view'], 'task')

    def test_service_failure_is_an_error_not_an_empty_result(self):
        self.server.seed('/global/keep.md', 'Keep', pinned=True)
        self.server.stop()
        for args in (('recall',), ('recall', '--query', 'keep'), ('status',), ('ls',), ('cat', '/global/keep.md')):
            result = self.run_client(*args, role='reader', ok=False)
            self.assertIn('Error:', result.stderr, args)


class HookTests(unittest.TestCase):
    """hooks/agent-memory-recall.py: one start-view recall per session, fail-open, INDEX.md fallback."""

    def setUp(self):
        self.server = start_server(self)
        self.server.seed('/global/keep.md', 'Keep it simple', 'Remove what is not needed.', pinned=True)
        self.home, self.env = self.server.home()
        git = ['git', '-c', 'user.name=t', '-c', 'user.email=t@t']
        self.repo = (self.server.root / 'checkout').resolve()
        subprocess.run([*git, 'init', '-q', str(self.repo)], check=True, env=self.env)
        subprocess.run([*git, '-C', str(self.repo), 'commit', '-q', '--allow-empty', '-m', 'x'], check=True, env=self.env)
        self.worktree = self.repo.parent / 'worktree'
        subprocess.run([*git, '-C', str(self.repo), 'worktree', 'add', '-q', str(self.worktree)], check=True, env=self.env)
        segments = '/'.join(self.repo.parts[1:])
        self.server.seed(f'/projects/{segments}/rule.md', 'Project rule', 'Run the project tests first.')

    def run_hook(self, cwd=None, stdin=None, env=None):
        event = json.dumps({'session_id': 's1', 'source': 'startup', 'hook_event_name': 'SessionStart',
                            'cwd': str(cwd or self.worktree)}) if stdin is None else stdin
        return subprocess.run([sys.executable, str(HOOK)], input=event, capture_output=True, text=True,
                              timeout=30, env=env or self.env)

    def hook_log(self):
        """Lines the hook appended to $HOME/.cache/agent-memory/recall-log.jsonl (its own record of failures)."""
        log = self.home / '.cache' / 'agent-memory' / 'recall-log.jsonl'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def check_log(self, line, via, project=None):
        self.assertEqual(set(line), {'ts', 'session_id', 'source', 'project', 'chars', 'ok', 'via'})
        self.assertEqual((line['session_id'], line['source'], line['via'], line['ok']),
                         ('s1', 'startup', via, via != 'none'))
        if project:
            self.assertEqual(line['project'], project)

    def context(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)['hookSpecificOutput']
        self.assertEqual(output['hookEventName'], 'SessionStart')
        return output['additionalContext']

    def test_injects_the_start_view_for_the_canonical_root_once(self):
        text = self.context(self.run_hook(cwd=self.worktree))
        self.assertIn(START_HEADING, text)
        self.assertIn('Remove what is not needed.', text)
        self.assertIn('Run the project tests first.', text)  # the worktree resolved to the checkout's project scope
        entries = self.server.recall_entries()
        self.assertEqual(len(entries), 1)
        self.assertEqual((entries[0]['client'], entries[0]['view'], entries[0]['project'], entries[0]['query']),
                         ('session-start', 'start', str(self.repo), ''))
        self.assertLess(len(text), 10000)  # Claude Code keeps only a 2,000-character preview above this
        [line] = self.hook_log()
        self.check_log(line, 'service', str(self.repo))
        self.assertGreater(line['chars'], 0)

    def test_service_down_injects_index_md(self):
        index = (self.server.mirror / 'INDEX.md').read_text()
        self.assertIn('keep.md', index)
        self.server.stop()
        text = self.context(self.run_hook())
        self.assertIn(index.strip(), text)
        self.assertEqual(self.server.recall_entries(), [])
        [line] = self.hook_log()
        self.check_log(line, 'index', str(self.repo))

    def test_hung_service_still_falls_back_within_the_timeout(self):
        index = (self.server.mirror / 'INDEX.md').read_text()
        hung = socket.socket()
        self.addCleanup(hung.close)
        hung.bind(('127.0.0.1', 0))
        hung.listen(8)  # accepts connections in the kernel but never answers
        (self.home / '.config/agent-memory/client.json').write_text(
            json.dumps({'url': 'http://127.0.0.1:%d' % hung.getsockname()[1], 'token': 'fixture-reader'}))
        started = time.monotonic()
        text = self.context(self.run_hook())
        self.assertLess(time.monotonic() - started, 20)
        self.assertIn(index.strip(), text)
        self.check_log(self.hook_log()[-1], 'index')

    def test_large_store_stays_under_claude_codes_10000_character_cap(self):
        for i in range(150):
            self.server.seed(f'/topics/bulk/item-{i:03d}.md', f'Bulk memory {i:03d} ' + 'x' * 100, 'Body.')
        live = self.context(self.run_hook())
        self.assertIn('Remove what is not needed.', live)
        self.assertLess(len(live), 10000)
        index = (self.server.mirror / 'INDEX.md').read_text()
        self.assertGreater(len(index), 20000)  # INDEX.md itself is never truncated
        self.server.stop()
        fallback = self.context(self.run_hook())
        self.assertLess(len(fallback), 10000)
        self.assertIn('Remove what is not needed.', fallback)  # pinned memories come first, so the cut keeps them
        self.assertIn('… truncated; full index: ', fallback)
        self.assertTrue(fallback.rstrip().endswith('INDEX.md'))
        cut = fallback.split('… truncated;')[0].rstrip().splitlines()[-1]
        self.assertIn(cut, index.splitlines())  # cut at a line boundary

    def test_no_service_and_no_mirror_prints_nothing(self):
        self.server.stop()
        for directory, names, _ in os.walk(self.server.root / 'mnt'):
            for name in names:
                os.chmod(os.path.join(directory, name), 0o755)
        shutil.rmtree(self.server.root / 'mnt')
        result = self.run_hook()
        self.assertEqual((result.returncode, result.stdout), (0, ''))
        [line] = self.hook_log()
        self.check_log(line, 'none', str(self.repo))
        self.assertEqual(line['chars'], 0)

    def test_fails_open(self):
        bare = {k: v for k, v in self.env.items() if not k.startswith('AGENT_MEMORY')}
        bare['HOME'] = str(self.server.root / 'empty')
        for result in (self.run_hook(stdin='not json'), self.run_hook(stdin=''),
                       self.run_hook(cwd=self.server.root / 'missing'),
                       self.run_hook(env=bare)):
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_hook(env=bare).stdout, '')  # no credentials and no mirror: silent, never an error


if __name__ == '__main__':
    unittest.main()
