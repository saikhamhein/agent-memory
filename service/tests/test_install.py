"""Installer migration tests with real SQLite/binary and simulated service controls."""
import importlib.util
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from service.tests.test_client import BINARY, SERVICE

spec = importlib.util.spec_from_file_location('install_user', SERVICE / 'install-user.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.database = Path(self.tmp.name) / 'memory.sqlite3'
        subprocess.run([str(BINARY), 'init', '--database', str(self.database)],
                       check=True, capture_output=True)
        self.db = sqlite3.connect(self.database)
        self.db.execute('PRAGMA journal_mode=WAL')
        self.db.execute("INSERT INTO observations VALUES ('fixture', 'test', '{}')")
        self.db.commit()
        self.before = list(self.db.iterdump())
        self.db.close()

    def test_migrate_wal_preserves_data_and_allows_python_maintenance(self):
        run = subprocess.run

        def command(args, **kwargs):
            if args[0] == 'systemctl':
                self.assertIn('is-active', args)
                return subprocess.CompletedProcess(args, 3)
            return run(args, **kwargs)

        with patch.object(installer.subprocess, 'run', side_effect=command):
            installer.migrate_database(BINARY, self.database)
        # A long-lived service connection no longer blocks the Python CLI's
        # journal pragma, even while both connections remain open.
        service = sqlite3.connect(self.database)
        self.addCleanup(service.close)
        self.assertEqual(service.execute('PRAGMA journal_mode').fetchone()[0], 'delete')
        self.assertEqual(list(service.iterdump()), self.before)
        python_cli = sqlite3.connect(self.database, timeout=0.1)
        self.addCleanup(python_cli.close)
        self.assertEqual(python_cli.execute('PRAGMA journal_mode=DELETE').fetchone()[0], 'delete')

    def test_active_service_is_stopped_then_restored_even_on_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail):
                service = sqlite3.connect(self.database)
                service.execute('PRAGMA journal_mode=WAL')
                service.execute('SELECT * FROM state').fetchall()
                self.addCleanup(service.close)
                events = []
                run = subprocess.run

                def command(args, **kwargs):
                    if args[0] == 'systemctl':
                        action = args[2]
                        events.append(action)
                        if action == 'stop':
                            service.close()
                        return subprocess.CompletedProcess(args, 0)
                    events.append('migrate')
                    if fail:
                        raise subprocess.CalledProcessError(1, args)
                    return run(args, **kwargs)

                with patch.object(installer.subprocess, 'run', side_effect=command):
                    if fail:
                        with self.assertRaises(subprocess.CalledProcessError):
                            installer.migrate_database(BINARY, self.database)
                    else:
                        installer.migrate_database(BINARY, self.database)
                self.assertEqual(events, ['is-active', 'stop', 'migrate', 'start'])

    def test_delete_mode_does_not_interrupt_service(self):
        with sqlite3.connect(self.database) as db:
            db.execute('PRAGMA journal_mode=DELETE')
        db.close()
        with patch.object(installer.subprocess, 'run') as run:
            installer.migrate_database(BINARY, self.database)
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
