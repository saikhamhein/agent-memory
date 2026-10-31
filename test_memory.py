"""memory.py is a stub that forwards `recall` to the HTTP client (design §3.3, §4.3 C.8).

The loader lines in ~/.codex/AGENTS.md and ~/.dsh/AGENTS.md run `python3 …/memory.py recall …`
and must keep working unchanged. The service is the only storage, so there is no storage to test here.
"""
from pathlib import Path
import subprocess
import sys
import unittest

from service.tests.harness import start_server

STUB = Path(__file__).resolve().parent / 'memory.py'
CLIENT = STUB.parent / 'service' / 'client.py'


class StubTests(unittest.TestCase):
    def setUp(self):
        self.server = start_server(self)
        self.server.seed('/global/keep.md', 'Keep it simple', 'Remove what is not needed.', pinned=True)
        self.server.seed('/global/zebrafish.md', 'Zebrafish husbandry', 'Zebrafish tanks need weekly water changes.')
        self.home, self.env = self.server.home()  # default client config under a throwaway HOME
        self.cwd = self.server.root / 'cwd'
        self.cwd.mkdir()

    def run_stub(self, *args):
        return subprocess.run([sys.executable, str(STUB), *args], capture_output=True, text=True,
                              timeout=30, env=self.env, cwd=self.cwd)

    def run_client(self, *args):
        return subprocess.run([sys.executable, str(CLIENT), *args], capture_output=True, text=True,
                              timeout=30, env=self.env, cwd=self.cwd)

    def test_old_loader_line_prints_the_clients_output(self):
        old_line = ['recall', '--project', '/home/sai/Projects/agent-memory', '--query', 'zebrafish husbandry',
                    '--limit', '12', '--budget', '12000']
        result = self.run_stub(*old_line)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Zebrafish tanks need weekly water changes.', result.stdout)
        self.assertEqual(result.stdout, self.run_client('recall', *old_line[1:]).stdout)
        self.assertNotIn('Remove what is not needed.', result.stdout)  # a task query, not the start view

    def test_start_view_and_client_label(self):
        result = self.run_stub('recall', '--client', 'session-start', '--project', '/home/sai/x')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith('# Shared agent memory — revision'))
        self.assertIn('Remove what is not needed.', result.stdout)
        self.assertEqual(result.stdout, self.run_client('recall', '--client', 'session-start', '--project', '/home/sai/x').stdout)
        self.assertEqual([e['client'] for e in self.server.recall_entries()], ['session-start', 'session-start'])
        self.assertEqual(self.run_stub('recall', '--query', 'kubernetes ingress yaml').stdout,
                         'No memory matches "kubernetes ingress yaml".\n')

    def test_other_commands_name_the_client_and_exit_2(self):
        candidate = self.server.root / 'candidate.json'
        candidate.write_text('{}')
        for args in (['status'], ['observe', str(candidate)], ['dream'], ['promote', str(candidate)],
                     ['rollback', '0'], ['export'], ['init'], ['validate', str(candidate)]):
            result = self.run_stub(*args)
            self.assertEqual(result.returncode, 2, args)
            self.assertIn('agent-memory-client', result.stderr, args)
            self.assertEqual(result.stdout, '', args)
        self.assertEqual(self.server.api('GET', '/v1/status')[1]['observations_pending'], 0)

    def test_service_down_is_an_error_not_an_empty_recall(self):
        self.server.stop()
        result = self.run_stub('recall', '--project', '/home/sai/x', '--limit', '12', '--budget', '12000')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, '')
        self.assertIn('Error:', result.stderr)

    def test_stub_creates_no_files(self):
        self.run_stub('recall')
        self.run_stub('status')
        self.assertEqual(list(self.cwd.iterdir()), [])  # no database, log or other file appears beside it


class StubSourceTests(unittest.TestCase):
    def test_stub_has_no_storage(self):
        source = STUB.read_text()
        self.assertNotIn('sqlite3', source)
        self.assertLessEqual(len(source.splitlines()), 40)  # design §3.3 calls for a ~20-line stub


if __name__ == '__main__':
    unittest.main()
