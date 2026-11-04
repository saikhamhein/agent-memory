"""memory.py is a stub that forwards `recall` to the HTTP client (design §3.3, §4.3 C.8).

The loader lines in ~/.codex/AGENTS.md and ~/.dsh/AGENTS.md run `python3 …/memory.py recall …`
and must keep working unchanged. The service is the only storage, so there is no storage to test here.
"""
from pathlib import Path
import subprocess
import sys
import unittest

from service.tests.harness import project_path, start_server

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
        old_line = ['recall', '--project', '/srv/projects/agent-memory', '--query', 'zebrafish husbandry',
                    '--limit', '12', '--budget', '12000']
        result = self.run_stub(*old_line)
        self.assertEqual(result.returncode, 0, result.stderr)
        # Loader lines always pass --query, so the stub prints the start view, one blank line, then the task view.
        start = self.run_client('recall', '--project', old_line[2], '--limit', '12', '--budget', '12000').stdout
        task = self.run_client('recall', *old_line[1:]).stdout
        self.assertEqual(result.stdout, start + '\n' + task)
        self.assertIn('Always apply', result.stdout)
        self.assertIn('Remove what is not needed.', result.stdout)  # the pinned core reaches Codex and DeepSeek
        self.assertIn('Zebrafish tanks need weekly water changes.', task)
        self.assertEqual([e['view'] for e in self.server.recall_entries()], ['start', 'task'] * 2)

    def test_old_global_root_flag_is_ignored(self):
        root = self.server.root / 'old-root'
        result = self.run_stub('--root', str(root), 'recall', '--query', 'zebrafish husbandry', '--limit', '12')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, self.run_client('recall').stdout + '\n' +
                         self.run_client('recall', '--query', 'zebrafish husbandry').stdout)
        self.assertFalse(root.exists())

    def test_project_memories_are_printed_once(self):
        root = '/srv/projects/zebra'
        self.server.seed(project_path(root, 'zebra-rule'), 'Zebrafish project rule', 'Zebrafish tanks here use RO water.')
        out = self.run_stub('recall', '--project', root, '--query', 'zebrafish husbandry').stdout
        self.assertEqual(out.count('### Zebrafish project rule'), 1)  # in the start view, not repeated in the task view
        self.assertEqual(out.count('Zebrafish tanks here use RO water.'), 1)
        self.assertIn('Zebrafish tanks need weekly water changes.', out)  # a match the start view lacks stays

    def test_nothing_new_in_the_task_view_prints_only_the_start_view(self):
        root = '/srv/projects/zebra'
        self.server.seed(project_path(root, 'axolotl-rule'), 'Axolotl care', 'Axolotls need cold water.')
        result = self.run_stub('recall', '--project', root, '--query', 'axolotl')
        start = self.run_client('recall', '--project', root).stdout
        self.assertEqual(result.stdout, start)  # no blank line and no empty task view
        self.assertEqual(result.stdout.count('Axolotls need cold water.'), 1)

    def test_start_view_and_client_label(self):
        result = self.run_stub('recall', '--client', 'session-start', '--project', '/srv/x')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith('# Shared agent memory — revision'))
        self.assertIn('Remove what is not needed.', result.stdout)
        self.assertEqual(result.stdout, self.run_client('recall', '--client', 'session-start', '--project', '/srv/x').stdout)
        self.assertEqual([e['client'] for e in self.server.recall_entries()], ['session-start', 'session-start'])
        unrelated = self.run_stub('recall', '--query', 'kubernetes ingress yaml').stdout
        self.assertTrue(unrelated.startswith('# Shared agent memory — revision'))  # start view, then the task view
        self.assertTrue(unrelated.endswith('\n\nNo memory matches "kubernetes ingress yaml".\n'))

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
        result = self.run_stub('recall', '--project', '/srv/x', '--limit', '12', '--budget', '12000')
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


class NoMachinePathsTests(unittest.TestCase):
    """The package holds code only; each machine keeps its own paths outside it."""

    def git(self, *args, ok=(0,)):
        ran = subprocess.run(['git', *args], cwd=STUB.parent, capture_output=True, text=True, timeout=30)
        self.assertIn(ran.returncode, ok, ran.stderr)
        return ran.stdout

    def test_no_tracked_file_names_a_home_directory(self):
        self.assertEqual(self.git('grep', '-n', '-E', '/home/\.?[a-z]', ok=(0, 1)), '')  # 1 means no match

    def test_no_tracked_path_is_a_home_directory(self):
        self.assertEqual([name for name in self.git('ls-files').splitlines() if '/home/' in name], [])


if __name__ == '__main__':
    unittest.main()
