import concurrent.futures
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

import memory


def item(name='example', kind='global', value=None):
    return {'id': name, 'title': name, 'body': 'Check existing evidence before asking.',
            'scope': {'kind': kind, **({'value': value} if value else {})},
            'confidence': 'explicit', 'tags': ['evidence'],
            'evidence': [{'source': 'test', 'locator': 'fixture:1', 'note': 'Test-only evidence'}]}


class MemoryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.db = memory.connect(self.root)

    def tearDown(self):
        self.db.close(); self.tmp.cleanup()

    def proposal(self, items=None):
        path = memory.new_dream(self.db, self.root)
        p = json.loads(path.read_text())
        p.update(memories=items or [item()], summary='Test revision', coverage='Fixture only', reviewed=True)
        memory.atomic_json(path, p)
        return path

    def test_stale_promotion_and_immutable_input(self):
        first, second = self.proposal(), self.proposal([item('second')])
        before = first.with_name('input.json').read_bytes()
        memory.promote(self.db, first)
        with self.assertRaisesRegex(ValueError, 'Stale'):
            memory.promote(self.db, second)
        self.assertEqual(before, first.with_name('input.json').read_bytes())
        self.assertEqual(0, json.loads(self.db.execute('SELECT body FROM revisions WHERE id=0').fetchone()[0]).__len__())

    def test_candidates_are_not_retrieved_and_late_observations_survive(self):
        self.db.execute('INSERT INTO observations VALUES (?,?,?)', ('a', memory.now(), memory.encode(item())))
        path = self.proposal()
        self.db.execute('INSERT INTO observations VALUES (?,?,?)', ('b', memory.now(), memory.encode(item('later'))))
        self.assertEqual([], memory.active(self.db)[1])
        memory.promote(self.db, path)
        next_path = memory.new_dream(self.db, self.root)
        snap = json.loads(next_path.with_name('input.json').read_text())
        self.assertEqual(['b'], [o['id'] for o in snap['observations']])

    def test_retrieval_scope_expiry_and_budget(self):
        values = [item(), item('project', 'project', '/projects/a'), item('topic', 'topic', 'burmese')]
        expired = item('old'); expired['expires'] = '2000-01-01'; values.append(expired)
        tentative = item('guess'); tentative['confidence'] = 'tentative'; values.append(tentative)
        self.assertEqual(['example'], [m['id'] for m in memory.retrieve(values, '/projects/ab')])
        selected = memory.retrieve(values, '/projects/a/src', topics=['burmese'])
        self.assertEqual({'example', 'project', 'topic'}, {m['id'] for m in selected})
        self.assertLessEqual(len(memory.render(selected, 1, 350)), 350)

    def test_concurrent_observations(self):
        def save(i):
            db = memory.connect(self.root)
            db.execute('INSERT INTO observations VALUES (?,?,?)', (str(i), memory.now(), memory.encode(item())))
            db.close()
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(save, range(32)))
        self.assertEqual(32, self.db.execute('SELECT COUNT(*) FROM observations').fetchone()[0])

    def test_readonly_commands_without_filesystem_write_access(self):
        proposal = self.proposal()
        memory.promote(self.db, proposal)
        self.db.close()
        database = self.root / 'memory.sqlite3'
        before = {p.name: (p.read_bytes(), p.stat().st_mtime_ns)
                  for p in self.root.iterdir() if p.is_file()}
        database.chmod(0o444)
        self.root.chmod(0o555)
        try:
            if os.access(self.root, os.W_OK) or os.access(database, os.W_OK):
                self.skipTest('This user can override filesystem permissions')
            command = [sys.executable, str(Path(memory.__file__)), '--root', str(self.root)]
            for args in (['recall', '--json'], ['status'], ['diff', str(proposal)],
                         ['validate', str(proposal)]):
                with self.subTest(command=args[0]):
                    result = subprocess.run(command + args, capture_output=True, text=True, timeout=5)
                    self.assertEqual(0, result.returncode, result.stderr)
                    if args[0] == 'recall':
                        self.assertEqual({'revision': 1, 'memories': [item()]}, json.loads(result.stdout))
            after = {p.name: (p.read_bytes(), p.stat().st_mtime_ns)
                     for p in self.root.iterdir() if p.is_file()}
            self.assertEqual(before, after)
        finally:
            self.root.chmod(0o700)
            database.chmod(0o600)

    def test_readonly_connection_rejects_writes_and_sees_new_commits(self):
        reader = memory.connect(self.root, readonly=True)
        try:
            with self.assertRaises(sqlite3.OperationalError):
                reader.execute("UPDATE state SET value=0 WHERE key='active'")
            self.db.execute('BEGIN IMMEDIATE')
            try:
                self.assertEqual((0, []), memory.active(reader))
            finally:
                self.db.rollback()
            memory.promote(self.db, self.proposal())
            self.assertEqual((1, [item()]), memory.active(reader))
        finally:
            reader.close()

    def test_readonly_commands_do_not_create_missing_store(self):
        root = self.root / 'missing'
        for command in ('recall', 'status'):
            result = subprocess.run([sys.executable, str(Path(memory.__file__)),
                                     '--root', str(root), command], capture_output=True)
            self.assertNotEqual(0, result.returncode)
            self.assertFalse(root.exists())

    def test_wal_migration_preserves_store(self):
        self.db.execute('PRAGMA journal_mode=WAL')
        memory.promote(self.db, self.proposal())
        before = list(self.db.iterdump())
        self.db.close()
        result = subprocess.run([sys.executable, str(Path(memory.__file__)),
                                 '--root', str(self.root), 'init'], capture_output=True, text=True)
        self.assertEqual(0, result.returncode, result.stderr)
        self.db = memory.connect(self.root, readonly=True)
        self.assertEqual('delete', self.db.execute('PRAGMA journal_mode').fetchone()[0])
        self.assertEqual(before, list(self.db.iterdump()))

    def test_readonly_uri_escapes_path(self):
        root = self.root / 'store ?#%'
        writer = memory.connect(root)
        writer.close()
        reader = memory.connect(root, readonly=True)
        try:
            self.assertEqual((0, []), memory.active(reader))
        finally:
            reader.close()

    def test_rollback_preserves_history_and_rejects_wrong_head(self):
        memory.promote(self.db, self.proposal())
        command = [sys.executable, str(Path(memory.__file__)), '--root', str(self.root),
                   'rollback', '0', '--reason', 'Test restore', '--expected']
        self.assertNotEqual(0, subprocess.run(command + ['0'], capture_output=True).returncode)
        self.assertEqual(0, subprocess.run(command + ['1'], capture_output=True).returncode)
        self.assertEqual((2, []), memory.active(self.db))
        self.assertEqual(3, self.db.execute('SELECT COUNT(*) FROM revisions').fetchone()[0])

    def test_validation_and_review_gates(self):
        invalid = item(); invalid['evidence'] = []
        with self.assertRaises(ValueError): memory.validate([invalid])
        with self.assertRaises(ValueError): memory.validate([item(), item()])
        secret = item(); secret['body'] = 'api_key=' + 'x' * 30
        with self.assertRaises(ValueError): memory.validate([secret])
        path = self.proposal(); p = json.loads(path.read_text()); p['reviewed'] = False
        memory.atomic_json(path, p)
        with self.assertRaises(ValueError): memory.promote(self.db, path)
        p['reviewed'] = True; p['conflicts'] = [{'status': 'unresolved'}]
        memory.atomic_json(path, p)
        with self.assertRaises(ValueError): memory.promote(self.db, path)


if __name__ == '__main__':
    unittest.main()
