"""agent-memory-client and the SessionStart hook against the real server (design §4.3 C), on a scratch store.

Standard library only, so these also run without the MCP virtual environment.
"""
import json
import os
import re
import socket
import shutil
import subprocess
import sys
import time
import unittest

from service.tests.harness import (BINARY, REPO, SERVICE, content, project_path, repo_with_worktree, sha,  # noqa: F401
                                   start_server)  # test_install imports BINARY and SERVICE from here

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

    def test_session_token_reads_and_suggests_only(self):
        """Without --config the client uses client.json: the agent-session token (recall + observe)."""
        self.server.seed('/global/keep.md', 'Keep', pinned=True)
        _, env = self.server.home()
        note = self.file('n.md', content('N'))
        output = self.server.api('POST', '/v1/dreams', {'sessions': []})[1]['output_store_id']  # a real non-live store

        def session(*args):
            return self.server.client(*args, role=None, env=env)
        self.assertEqual(json.loads(session('status').stdout)['revision'], 1)
        self.assertEqual(json.loads(session('ls').stdout)['data'][0]['path'], '/global/keep.md')
        self.assertIn('Check existing evidence', session('cat', '/global/keep.md').stdout)
        self.assertEqual(session('observe', '/global/n.md', note).returncode, 0)
        for args in (('write', '/global/n.md', note), ('rm', '/global/keep.md'), ('versions',),
                     ('redact', 'memver_' + '0' * 32), ('dream', 'start'), ('dream', 'show'),
                     ('ls', '--store', output)):
            result = session(*args)
            self.assertNotEqual(result.returncode, 0, args)
            self.assertIn('403', result.stderr, args)
        unknown = session('ls', '--store', 'memstore_' + '0' * 32)  # the store is resolved before the capability check
        self.assertNotEqual(unknown.returncode, 0)
        self.assertIn('404', unknown.stderr)
        self.assertIn('not_found', unknown.stderr)
        self.assertIn('403', self.run_client('observe', '/global/m.md', note, role='reader', ok=False).stderr)
        self.assertEqual(self.json('status')['memories'], 1)
        self.assertEqual(self.json('status')['observations_pending'], 1)

    def test_useful_logs_a_mark_and_changes_nothing(self):
        self.server.seed('/global/keep.md', 'Keep')
        before = self.json('status')
        marked = self.run_client('useful', '/global/keep.md', '--why', 'It stopped a guess', role='agent-session')
        self.assertEqual(json.loads(marked.stdout)['path'], '/global/keep.md')
        [mark] = [e for e in self.server.recall_entries() if e['view'] == 'useful']
        self.assertEqual((mark['client'], mark['path'], mark['why']), ('agent-session', '/global/keep.md', 'It stopped a guess'))
        self.assertEqual(self.json('status'), before)
        self.assertIn('404', self.run_client('useful', '/global/missing.md', '--why', 'w', role='agent-session', ok=False).stderr)
        self.assertIn('403', self.run_client('useful', '/global/keep.md', '--why', 'w', role='reader', ok=False).stderr)
        self.assertEqual(len(self.server.recall_entries()), 1)

    def test_dream_workflow(self):
        self.server.seed('/global/existing.md', 'Existing', 'Original body.')
        self.run_client('observe', '/global/candidate.md', self.file('c.md', content('Candidate')), role='agent-session')
        sessions = self.file('sessions.json', json.dumps([{'source': 'claude-code', 'locator': '/x/a.jsonl'}]))
        started = self.json('dream', 'start', '--sessions', sessions)
        self.assertEqual((started['status'], len(started['observations']), len(started['sessions'])), ('running', 1, 1))
        output = started['output_store_id']
        # X10: one running dream; the 409 names it so a stale run can be found and cancelled.
        clash = self.run_client('dream', 'start', ok=False)
        self.assertIn('dream_running', clash.stderr)
        self.assertIn(started['id'], clash.stderr)
        added = self.file('z.md', content('Zebrafish husbandry', 'Zebrafish tanks need weekly water changes.'))
        self.run_client('write', '/global/zebrafish.md', added, '--store', output)
        shown = self.json('dream', 'show', started['id'], '--diff')
        self.assertEqual([m['path'] for m in shown['diff']['added']], ['/global/zebrafish.md'])
        self.assertIn(started['id'], [d['id'] for d in self.json('dream', 'show')['data']])
        # X14: the review is {summary, coverage, conflicts}; a `reviewed` key is rejected.
        stale = self.file('old-review.json', json.dumps(dict(summary='s', coverage='c', conflicts=[], reviewed=True)))
        self.assertIn('400', self.run_client('dream', 'adopt', started['id'], stale, ok=False).stderr)
        review = self.file('review.json', json.dumps(dict(summary='Add zebrafish', coverage='client test', conflicts=[])))
        done = self.json('dream', 'adopt', started['id'], review)
        self.assertEqual((done['status'], done['adoption']['summary']), ('completed', 'Add zebrafish'))
        # X7: the output store is purged when the dream ends, so no leaked secret survives in it.
        self.assertIn('400', self.run_client('dream', 'show', started['id'], '--diff', ok=False).stderr)
        self.assertEqual(self.json('ls', '--store', output)['data'], [])
        self.run_client('cat', '/global/zebrafish.md', '--store', output, ok=False)
        self.assertIn('Zebrafish tanks need weekly water changes.', self.run_client('recall', '--query', 'zebrafish husbandry').stdout)
        self.run_client('dream', 'adopt', started['id'], review, ok=False)  # only a running dream can be adopted
        second = self.json('dream', 'start')
        self.assertEqual(second['observations'], [])  # adopted: the observation is no longer pending
        self.assertEqual(self.json('dream', 'cancel', second['id'])['status'], 'canceled')
        self.assertEqual(self.json('dream', 'show', second['id'])['status'], 'canceled')
        self.assertEqual(self.json('ls', '--store', second['output_store_id'])['data'], [])

    def test_dream_cycle_with_nothing_to_change_is_cancelled(self):
        self.server.seed('/global/existing.md', 'Existing')
        started = self.json('dream', 'start')
        self.assertEqual(started['observations'], [])
        shown = self.json('dream', 'show', started['id'], '--diff')
        self.assertEqual(shown['diff'], {'added': [], 'removed': [], 'changed': []})
        self.assertEqual(self.json('status')['dreams_running'], 1)
        self.json('dream', 'cancel', started['id'])
        self.assertEqual(self.json('status')['dreams_running'], 0)
        self.assertEqual(self.json('dream', 'start')['status'], 'running')  # the slot is free again

    def test_stale_dream_is_found_cancelled_and_replaced(self):
        """A run that died leaves a running dream; the 409 names it so the next run can cancel it (X10)."""
        stale = self.json('dream', 'start')
        clash = self.run_client('dream', 'start', ok=False)
        [named] = re.findall(r'drm_[0-9a-f]{32}', clash.stderr)
        self.assertEqual(named, stale['id'])
        shown = self.json('dream', 'show', named)
        self.assertEqual(shown['status'], 'running')
        self.assertTrue(shown['created_at'])  # what the skill compares with "more than an hour ago"
        self.json('dream', 'cancel', named)
        fresh = self.json('dream', 'start')
        self.assertNotEqual(fresh['id'], stale['id'])
        self.assertEqual(self.json('status')['dreams_running'], 1)

    def test_recall_canonicalizes_worktrees_and_has_no_project_id(self):
        _, env = self.server.home()
        repo, worktree = repo_with_worktree(self.server.root, env)
        (worktree / 'sub').mkdir()
        self.server.seed(project_path(repo, 'repo-rule'), 'Repo rule', 'Repo root rule text.')
        self.server.seed(project_path(repo / 'sub', 'sub-rule'), 'Sub rule', 'Subdirectory rule text.')

        def recall(project):
            result = self.server.client('recall', '--project', project, role=None, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout, self.server.recall_entries()[-1]['project']
        text, logged = recall(str(worktree))  # a linked worktree maps to its main checkout
        self.assertEqual(logged, str(repo))
        self.assertIn('Repo root rule text.', text)
        self.assertNotIn('Subdirectory rule text.', text)
        text, logged = recall(str(worktree / 'sub'))  # the same relative directory in the main checkout
        self.assertEqual(logged, str(repo / 'sub'))
        self.assertIn('Subdirectory rule text.', text)
        self.assertIn('Repo root rule text.', text)
        text, logged = recall(str(repo))
        self.assertEqual(logged, str(repo))
        plain = self.server.root / 'not-a-repo'
        plain.mkdir()
        self.assertEqual(recall(str(plain))[1], str(plain))  # a non-git path is sent unchanged
        gone = self.server.client('recall', '--project-id', 'agent-memory', role=None, env=env)
        self.assertEqual(gone.returncode, 2)

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
        self.repo, self.worktree = repo_with_worktree(self.server.root, self.env)
        self.server.seed(project_path(self.repo), 'Project rule', 'Run the project tests first.')

    def run_hook(self, cwd=None, stdin=None, env=None, source='startup'):
        event = json.dumps({'session_id': 's1', 'source': source, 'hook_event_name': 'SessionStart',
                            'cwd': str(cwd or self.worktree)}) if stdin is None else stdin
        return subprocess.run([sys.executable, str(HOOK)], input=event, capture_output=True, text=True,
                              timeout=30, env=env or self.env)

    def hook_log(self):
        """Lines the hook appended to $HOME/.cache/agent-memory/recall-log.jsonl (its own record of failures)."""
        log = self.home / '.cache' / 'agent-memory' / 'recall-log.jsonl'
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    def check_log(self, line, via, project=None, source='startup'):
        self.assertEqual(set(line), {'ts', 'session_id', 'source', 'project', 'chars', 'ok', 'via'})
        self.assertEqual((line['session_id'], line['source'], line['via'], line['ok']),
                         ('s1', source, via, via != 'none'))
        if project:
            self.assertEqual(line['project'], project)

    def context(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout, 'the hook printed nothing')
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

    def test_resume_and_fork_inject_nothing_but_are_logged(self):
        for source in ('resume', 'fork'):
            result = self.run_hook(source=source)
            self.assertEqual((result.returncode, result.stdout), (0, ''), source)
            line = self.hook_log()[-1]
            self.check_log(line, 'skipped', source=source)
            self.assertEqual(line['chars'], 0)
        self.assertEqual(len(self.hook_log()), 2)
        self.assertEqual(self.server.recall_entries(), [])  # the skipped runs never called the service
        self.context(self.run_hook(source='startup'))  # other sources still inject
        self.assertEqual(len(self.server.recall_entries()), 1)

    def test_worktree_subdirectory_maps_to_the_same_directory_in_the_checkout(self):
        (self.worktree / 'sub').mkdir()
        self.server.seed(project_path(self.repo / 'sub', 'sub-rule'), 'Sub rule', 'Subdirectory rule text.')
        text = self.context(self.run_hook(cwd=self.worktree / 'sub'))
        self.assertIn('Subdirectory rule text.', text)
        [line] = self.hook_log()
        self.check_log(line, 'service', str(self.repo / 'sub'))
        self.assertEqual(self.server.recall_entries()[-1]['project'], str(self.repo / 'sub'))

    def test_service_down_injects_index_md_with_a_banner(self):
        index = (self.server.mirror / 'INDEX.md').read_text()
        self.assertIn('keep.md', index)
        self.server.stop()
        text = self.context(self.run_hook())
        banner = ('The memory service is unavailable; this is the mirror index. '
                  'Use the mirror files until it is back.\n\n')
        self.assertTrue(text.startswith(banner), text[:200])
        self.assertIn(index.strip(), text)
        self.assertEqual(self.server.recall_entries(), [])
        [line] = self.hook_log()
        self.check_log(line, 'index', str(self.repo))

    def test_mirror_is_next_to_the_database_whatever_server_toml_says(self):
        config = self.home / '.config/agent-memory/server.toml'
        config.write_text('mount = "%s"\n' % (self.server.root / 'elsewhere') + config.read_text())  # the key is gone (X16)
        index = (self.server.mirror / 'INDEX.md').read_text()
        self.server.stop()
        self.assertIn(index.strip(), self.context(self.run_hook()))

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
        bare = {**self.env, 'HOME': str(self.server.root / 'empty')}
        for result in (self.run_hook(stdin='not json'), self.run_hook(stdin=''),
                       self.run_hook(cwd=self.server.root / 'missing'),
                       self.run_hook(env=bare)):
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.run_hook(env=bare).stdout, '')  # no credentials and no mirror: silent, never an error


if __name__ == '__main__':
    unittest.main()
