"""Installer migration tests with real SQLite/binary and simulated service controls."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from service.tests.test_client import BINARY, SERVICE

spec = importlib.util.spec_from_file_location('install_user', SERVICE / 'install-user.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)


# --- v2 migration guards (design §3.6, §4.3 "M" tests 8-10) -------------------------------------------------------
# The installer and update-local.sh run for real, but only inside a throwaway layout: a copy of service/, a fake HOME,
# and fake systemctl/systemd-analyze/git/cargo first on PATH. They never touch ~/.config, ~/.local/bin, the systemd
# units, the live database or the repository's own runtime/ directory.

V1_DDL = '''
    CREATE TABLE revisions (id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
        reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
    CREATE TABLE state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
    CREATE TABLE observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
    CREATE TABLE dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
        input TEXT NOT NULL, promoted INTEGER, proposal TEXT);
'''
LEGACY_TABLES = (('revisions', 'id'), ('state', 'key'), ('observations', 'id'), ('dreams', 'id'))


def encode(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2)


def v1_memory(slug, scope, body):
    return {'id': slug, 'title': 'Title of ' + slug, 'body': body, 'scope': scope, 'confidence': 'explicit',
            'tags': ['fixture'], 'evidence': [{'source': 'test', 'locator': 'fixture:1', 'note': 'Test evidence'}]}


def v1_database(path, revisions):
    """A real v1 store: revision 0 empty, then the given memory lists as revisions 1..n, plus one observation."""
    db = sqlite3.connect(path)
    db.execute('PRAGMA journal_mode=DELETE')
    db.executescript(V1_DDL)
    rows = [[]] + list(revisions)
    for number, memories in enumerate(rows):
        body = encode(memories)
        db.execute('INSERT INTO revisions VALUES (?,?,?,?,?,?)', (
            number, None if number == 0 else number - 1, f'2026-10-0{number + 1}T10:00:00+00:00',
            f'Revision {number}', body, hashlib.sha256(body.encode()).hexdigest()))
    db.execute("INSERT INTO state VALUES ('active', ?)", (len(rows) - 1,))
    db.execute("INSERT INTO observations VALUES ('obs-1', '2026-10-01T00:00:00+00:00', ?)",
               (encode(v1_memory('obs-1', {'kind': 'global'}, 'Observed')),))
    db.commit()
    db.close()


def dump(path):
    with sqlite3.connect(path) as db:
        text = list(db.iterdump())
    return text


def legacy_rows(path):
    with sqlite3.connect(path) as db:
        return [db.execute(f'SELECT * FROM {table} ORDER BY {key}').fetchall() for table, key in LEGACY_TABLES]


def user_version(path):
    with sqlite3.connect(path) as db:
        return db.execute('PRAGMA user_version').fetchone()[0]


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


FAKE_SYSTEMCTL = '''#!{python}
import os, signal, subprocess, sys, time
from pathlib import Path
state, home = Path({state!r}), Path({home!r})
args = sys.argv[1:]
with open({events!r}, 'a') as log:
    log.write('systemctl ' + ' '.join(args) + '\\n')
assert args[0] == '--user', args
action, active, pidfile = args[1], state / 'active', state / 'pid'

def stop_server():
    if not pidfile.exists():
        return
    pid = int(pidfile.read_text())
    pidfile.unlink()
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    for _ in range(100):
        try:
            if Path(f'/proc/{{pid}}/stat').read_text().rsplit(')', 1)[1].split()[0] == 'Z':
                return
        except FileNotFoundError:
            return
        time.sleep(0.05)

if action == 'is-active':
    sys.exit(0 if active.exists() else 3)
if action == 'stop':
    stop_server()
    active.unlink(missing_ok=True)
elif action in ('start', 'restart'):
    stop_server()
    active.write_text('1')
    installed = home / '.local/bin/agent-memory-server'
    binary = installed if installed.exists() else Path({wrapper!r})
    process = subprocess.Popen([str(binary), 'serve', '--config', str(home / '.config/agent-memory/server.toml')],
                               stdin=subprocess.DEVNULL, stdout=open(state / 'server.log', 'ab'),
                               stderr=subprocess.STDOUT, start_new_session=True)
    pidfile.write_text(str(process.pid))
'''

# Records every server invocation; at `migrate` it also records which files already exist under runtime/ (the backup).
FAKE_SERVER = '''#!/bin/sh
echo "binary $*" >> {events}
if [ "$1" = migrate ]; then
  echo "runtime-files-at-migrate: $(find {runtime} -type f 2>/dev/null | sort | tr '\\n' ' ')" >> {events}
fi
exec {real} "$@"
'''


class Sandbox:
    """A copy of service/ in a temporary repo layout with fake HOME and fake system tools."""

    def __init__(self, test):
        self.tmp = tempfile.TemporaryDirectory()
        test.addCleanup(self.tmp.cleanup)
        test.addCleanup(self.stop_server)
        root = Path(self.tmp.name).resolve()
        self.test, self.repo, self.home, self.state = test, root / 'repo', root / 'home', root / 'state'
        self.events_file, self.fakebin = root / 'events.log', root / 'fakebin'
        self.database = self.repo / 'memory.sqlite3'
        self.runtime = self.repo / 'runtime'
        for directory in (self.repo, self.home, self.state, self.fakebin):
            directory.mkdir(parents=True)
        self.events_file.touch()
        service = self.repo / 'service'
        shutil.copytree(SERVICE, service, ignore=shutil.ignore_patterns('target', 'tests', '__pycache__', '.venv-mcp'))
        (service / 'install-mcp.py').write_text('print("install-mcp stub")\n')  # the real one needs uv and network
        self.wrapper = service / 'target/release/agent-memory-server'
        self.wrapper.parent.mkdir(parents=True)
        self.write_tool(self.wrapper, FAKE_SERVER.format(events=self.events_file, runtime=self.runtime, real=BINARY))
        script = SERVICE.parent / 'update-local.sh'
        if script.exists():
            shutil.copyfile(script, self.repo / 'update-local.sh')
            (self.repo / 'update-local.sh').chmod(0o755)
        python = sys.executable
        self.write_tool(self.fakebin / 'systemctl', FAKE_SYSTEMCTL.format(
            python=python, state=str(self.state), home=str(self.home), events=str(self.events_file),
            wrapper=str(self.wrapper)))
        self.write_tool(self.fakebin / 'systemd-analyze', '#!/bin/sh\necho "systemd-analyze $*" >> %s\n' % self.events_file)
        self.write_tool(self.fakebin / 'cargo', '#!/bin/sh\necho "cargo $*" >> %s\n' % self.events_file)
        self.write_tool(self.fakebin / 'git', '#!/bin/sh\necho "git $*" >> %s\n'
                        'case "$1 $2" in "branch --show-current") echo master;; "rev-parse --short") echo abc1234;; esac\n'
                        % self.events_file)
        self.write_tool(self.fakebin / 'python3', '#!/bin/sh\necho "python3 $*" >> %s\nexec %s "$@"\n'
                        % (self.events_file, python))
        self.env = dict(os.environ, HOME=str(self.home), PATH=f'{self.fakebin}:{os.environ["PATH"]}')
        for name in ('CODEX_HOME', 'XDG_CONFIG_HOME'):
            self.env.pop(name, None)
        for tool in ('systemctl', 'systemd-analyze', 'git', 'cargo', 'python3'):
            test.assertEqual(shutil.which(tool, path=self.env['PATH']), str(self.fakebin / tool))
        # Existing, already-installed credentials and config, as on the real machine; the port is private.
        port = free_port()
        reader, maintainer = 'fixture-reader', 'fixture-maintainer'
        config = self.home / '.config/agent-memory'
        config.mkdir(parents=True, mode=0o700)
        (config / 'server.toml').write_text(
            f'listen = "127.0.0.1:{port}"\ndatabase = {json.dumps(str(self.database))}\n'
            f'[[tokens]]\nname = "agent-recall"\nsha256 = "{hashlib.sha256(reader.encode()).hexdigest()}"\n'
            'capabilities = ["recall"]\n'
            f'[[tokens]]\nname = "memory-maintainer"\nsha256 = "{hashlib.sha256(maintainer.encode()).hexdigest()}"\n'
            'capabilities = ["recall", "observe", "review", "promote"]\n')
        for name, token in (('client.json', reader), ('maintainer.json', maintainer)):
            (config / name).write_text(json.dumps({'url': f'http://127.0.0.1:{port}', 'token': token}))
        (self.state / 'active').write_text('1')  # the previous version of the service is running

    @staticmethod
    def write_tool(path, text):
        path.write_text(text)
        path.chmod(0o755)

    def stop_server(self):
        pidfile = self.state / 'pid'
        if pidfile.exists():
            try:
                os.kill(int(pidfile.read_text()), 9)
            except ProcessLookupError:
                pass

    def events(self):
        return self.events_file.read_text().splitlines()

    def service_active(self):
        return (self.state / 'active').exists()

    def installed(self):
        return [path for path in (self.home / '.local/bin/agent-memory-server',
                                  self.home / '.config/systemd/user/agent-memory.service') if path.exists()]

    def run(self, *command):
        return subprocess.run(command, cwd=self.repo, env=self.env, capture_output=True, text=True, timeout=120)

    def install(self, *extra):
        return self.run(sys.executable, str(self.repo / 'service/install-user.py'), '--database', str(self.database), *extra)

    def migrated_database(self, revisions):
        v1_database(self.database, revisions)
        subprocess.run([str(BINARY), 'migrate', '--database', str(self.database)], check=True, capture_output=True)


REVISIONS = [
    [v1_memory('keep-it-simple', {'kind': 'global'}, 'Remove what is not needed. မြန်မာ'),
     v1_memory('hardware-note', {'kind': 'topic', 'value': 'hardware'}, 'Pi notes')],
    [v1_memory('keep-it-simple', {'kind': 'global'}, 'Remove what is not needed, v2. မြန်မာ'),
     v1_memory('hardware-note', {'kind': 'topic', 'value': 'hardware'}, 'Pi notes')],
]
OFFENDING = [[v1_memory('bad', {'kind': 'topic', 'value': 'machine learning'}, 'x')]]


class InstallMigrationGuardTests(unittest.TestCase):
    def setUp(self):
        self.box = Sandbox(self)

    def test_install_refuses_v1_without_flag(self):
        v1_database(self.box.database, REVISIONS)
        before = self.box.database.read_bytes()
        ran = self.box.install()
        self.assertNotEqual(ran.returncode, 0, ran.stdout)
        self.assertIn('--migrate', ran.stdout + ran.stderr)
        self.assertEqual(self.box.database.read_bytes(), before)
        self.assertEqual(self.box.installed(), [], 'nothing may be installed over a v1 database')
        self.assertTrue(self.box.service_active(), 'the service must be left as it was found')
        self.assertFalse([e for e in self.box.events() if e.startswith('binary migrate')])

    def test_install_migrate_backs_up_first(self):
        v1_database(self.box.database, REVISIONS)
        original = dump(self.box.database)
        original_rows = legacy_rows(self.box.database)
        ran = self.box.install('--migrate', '--pin', 'keep-it-simple')
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        events = self.box.events()
        migrate = next(i for i, e in enumerate(events) if e.startswith('binary migrate'))
        self.assertIn('--database ' + str(self.box.database), events[migrate])
        self.assertIn('--pin keep-it-simple', events[migrate], 'the installer passes --pin through')
        self.assertIn('systemctl --user stop agent-memory.service', events[:migrate])
        # A good, v1, integrity-checked backup under runtime/ existed before migrate started.
        listed = events[migrate + 1]
        self.assertTrue(listed.startswith('runtime-files-at-migrate: '), listed)
        backups = []
        for name in listed.split(': ', 1)[1].split():
            path = Path(name)
            self.assertTrue(str(path).startswith(str(self.box.runtime) + '/'), path)
            try:
                with sqlite3.connect(path) as db:
                    ok = db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
                if ok and dump(path) == original:
                    backups.append(path)
            except sqlite3.DatabaseError:
                pass  # not a database (e.g. a report)
        self.assertTrue(backups, f'no integrity-checked copy of the v1 database among {listed}')
        # Then migrated, installed, restarted and verified.
        self.assertEqual(user_version(self.box.database), 2)
        self.assertEqual(legacy_rows(self.box.database), original_rows)
        with sqlite3.connect(self.box.database) as db:
            content = db.execute("SELECT content FROM memories WHERE path='/global/keep-it-simple.md'").fetchone()[0]
        self.assertIn('"pinned": true', content)
        self.assertTrue(any(e.startswith(('systemctl --user restart', 'systemctl --user start')) for e in events[migrate:]))
        self.assertEqual(len(self.box.installed()), 2)
        self.assertTrue(self.box.service_active())
        self.assertEqual(user_version(backups[0]), 0, 'the backup stays a v1 database')

    def test_install_migrate_failure_restores_everything(self):
        v1_database(self.box.database, OFFENDING)
        before = self.box.database.read_bytes()
        ran = self.box.install('--migrate')
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('machine learning', ran.stdout + ran.stderr, 'the offender is shown to the user')
        self.assertEqual(self.box.database.read_bytes(), before)
        self.assertEqual(self.box.installed(), [])
        self.assertTrue(self.box.service_active(), 'the stopped service is started again')


class UpdateLocalTests(unittest.TestCase):
    def setUp(self):
        self.box = Sandbox(self)
        if not (self.box.repo / 'update-local.sh').exists():
            self.fail('update-local.sh does not exist')

    def installer_calls(self):
        return [e for e in self.box.events() if e.startswith('python3 ') and 'install-user.py' in e]

    def test_update_local_never_migrates(self):
        lines = (self.box.repo / 'update-local.sh').read_text().splitlines()
        self.assertFalse([line for line in lines if 'install-user.py' in line and '--migrate' in line])
        v1_database(self.box.database, REVISIONS)
        before = self.box.database.read_bytes()
        ran = self.box.run('./update-local.sh')
        self.assertNotEqual(ran.returncode, 0, 'the v1 guard must stop the update')
        self.assertTrue(self.installer_calls(), 'it must reach the installer, not fail earlier:\n' + ran.stderr)
        self.assertIn('--migrate', ran.stdout + ran.stderr, 'the installer says how to migrate')
        self.assertFalse([e for e in self.box.events() if '--migrate' in e or e.startswith('binary migrate')])
        self.assertFalse([e for e in self.box.events() if 'install-mcp' in e], 'nothing runs after the guard')
        self.assertEqual(self.box.database.read_bytes(), before)
        self.assertEqual(self.box.installed(), [])

    def test_update_local_updates_a_migrated_store(self):
        self.box.migrated_database(REVISIONS)
        ran = self.box.run('./update-local.sh')
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertTrue(self.installer_calls())
        self.assertFalse([e for e in self.box.events() if '--migrate' in e or e.startswith('binary migrate')])
        self.assertEqual(user_version(self.box.database), 2)
        self.assertTrue(self.box.service_active())


if __name__ == '__main__':
    unittest.main()
