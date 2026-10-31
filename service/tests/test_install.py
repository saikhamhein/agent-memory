"""Installer and update-script tests: real SQLite and binary, simulated systemd."""
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SERVICE = Path(__file__).resolve().parents[1]
BINARY = SERVICE / 'target/release/agent-memory-server'
spec = importlib.util.spec_from_file_location('install_user', SERVICE / 'install-user.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)

MEMORY = {'id': 'keep-it-simple', 'scope': {'kind': 'global'}, 'title': 'Keep it simple',
          'body': 'Remove what is not needed.', 'confidence': 'explicit', 'tags': [],
          'evidence': [{'source': 'test', 'locator': 'fixture', 'note': 'synthetic'}]}


def v1_store(path):
    """A v1 store with one revision, written with the v1 DDL."""
    encode = lambda v: json.dumps(v, ensure_ascii=False, sort_keys=True, indent=2)
    with sqlite3.connect(path) as db:
        db.executescript('''
            CREATE TABLE revisions (id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
                reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
            CREATE TABLE state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            CREATE TABLE observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
            CREATE TABLE dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
                input TEXT NOT NULL, promoted INTEGER, proposal TEXT);''')
        for n, body in enumerate([[], [MEMORY]]):
            db.execute('INSERT INTO revisions VALUES (?,?,?,?,?,?)',
                       (n, n - 1 if n else None, f'2026-10-0{n + 1}T00:00:00+00:00', 'r', encode(body), 'x'))
        db.execute("INSERT INTO state VALUES ('active', 1)")
    db.close()


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.database = Path(self.tmp.name) / 'memory.sqlite3'
        v1_store(self.database)
        self.bytes = self.database.read_bytes()

    def test_install_refuses_v1_without_flag(self):
        with patch.object(installer.subprocess, 'run') as run, \
                patch.object(sys, 'argv', ['install-user.py', '--database', str(self.database)]), \
                patch.object(installer.Path, 'home', return_value=Path(self.tmp.name) / 'home'):
            with self.assertRaises(SystemExit) as exit:
                installer.main()
            run.assert_not_called()
        self.assertNotEqual(exit.exception.code, 0)
        self.assertEqual(self.database.read_bytes(), self.bytes)
        self.assertFalse((Path(self.tmp.name) / 'home').exists())

    def fake_systemd(self, active, events, fail_migrate=False):
        run = subprocess.run

        def command(args, **kwargs):
            if args[0] == 'systemctl':
                events.append(args[2])
                return subprocess.CompletedProcess(args, 0 if active else 3)
            events.append(args[1])
            if args[1] == 'migrate' and fail_migrate:
                raise subprocess.CalledProcessError(1, args)
            return run(args, **kwargs)
        return command

    def test_install_migrate_backs_up_first(self):
        events = []
        with patch.object(installer.subprocess, 'run', side_effect=self.fake_systemd(True, events)):
            backup = installer.migrate_store(BINARY, self.database, ['keep-it-simple'])
        self.assertEqual(events, ['is-active', 'stop', 'backup', 'migrate'])
        # The backup is the untouched v1 store; the original is now v2 with the pin applied.
        self.assertEqual(backup.parent, self.database.parent / 'runtime')
        self.assertEqual(installer.schema_version(backup), 0)
        with sqlite3.connect(backup) as db:
            self.assertEqual(db.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
            self.assertEqual(db.execute('SELECT COUNT(*) FROM revisions').fetchone()[0], 2)
        db.close()
        self.assertEqual(installer.schema_version(self.database), 2)
        with sqlite3.connect(self.database) as db:
            content = db.execute('SELECT content FROM memories').fetchone()[0]
        db.close()
        self.assertIn('"pinned": true', content)

    def test_failed_migration_restores_service_and_store(self):
        for active in (True, False):
            with self.subTest(active=active):
                events = []
                fake = self.fake_systemd(active, events, fail_migrate=True)
                with patch.object(installer.subprocess, 'run', side_effect=fake):
                    with self.assertRaises(subprocess.CalledProcessError):
                        installer.migrate_store(BINARY, self.database, [])
                expected = ['is-active', 'stop', 'backup', 'migrate', 'start'] if active \
                    else ['is-active', 'backup', 'migrate']
                self.assertEqual(events, expected)
                self.assertEqual(self.database.read_bytes(), self.bytes)


class UpdateLocalTests(unittest.TestCase):
    def test_update_local_never_migrates(self):
        script = SERVICE.parent / 'update-local.sh'
        self.assertNotIn('--migrate', script.read_text())
        with tempfile.TemporaryDirectory() as tmp:
            fake, log = Path(tmp) / 'bin', Path(tmp) / 'calls.log'
            fake.mkdir()
            for name in ('git', 'cargo', 'python3'):
                tool = fake / name
                reply = 'master' if name == 'git' else ''
                tool.write_text(f'#!/bin/sh\necho "{name} $*" >> "{log}"\n'
                                f'case "$1" in branch|rev-parse) echo {reply or "x"};; esac\n')
                tool.chmod(0o755)
            env = dict(os.environ, PATH=f'{fake}:/usr/bin:/bin', HOME=tmp)
            subprocess.run(['sh', str(script)], env=env, check=True, capture_output=True)
            calls = log.read_text().splitlines()
        installs = [c for c in calls if 'install-user.py' in c]
        self.assertEqual(len(installs), 1)
        self.assertTrue(all('--migrate' not in c for c in calls))


if __name__ == '__main__':
    unittest.main()
