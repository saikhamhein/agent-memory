"""Installer migration tests with real SQLite/binary and simulated service controls."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
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
action, active, pidfile, enabled = args[1], state / 'active', state / 'pid', state / 'enabled'
failing = state / 'fail' / 'systemctl'  # fail_tool: empty file = every action, else the one named action
if failing.exists() and failing.read_text() in ('', action):
    sys.exit(1)

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
if action == 'is-enabled':
    sys.exit(0 if enabled.exists() else 1)
if action == 'enable':
    enabled.write_text('1')
if action == 'disable':
    enabled.unlink(missing_ok=True)
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
# With state/crash-after-commit it runs the real migrate and then exits non-zero, as if the process died after COMMIT.
FAKE_SERVER = '''#!/bin/sh
echo "binary $*" >> {events}
if [ "$1" = migrate ]; then
  echo "runtime-files-at-migrate: $(find {runtime} -type f 2>/dev/null | sort | tr '\\n' ' ')" >> {events}
  if [ -e {state}/crash-after-commit ]; then
    {real} "$@"
    status=$?
    [ $status -eq 0 ] && status=70
    exit $status
  fi
fi
exec {real} "$@"
'''
FAKE_TOOL = '''#!/bin/sh
echo "{name} $*" >> {events}
if [ -e {state}/fail/{name} ]; then exit 1; fi
{body}'''


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
        for directory in (self.repo, self.home, self.state, self.state / 'fail', self.fakebin):
            directory.mkdir(parents=True)
        self.events_file.touch()
        service = self.repo / 'service'
        shutil.copytree(SERVICE, service, ignore=shutil.ignore_patterns('target', 'tests', '__pycache__'))
        self.wrapper = service / 'target/release/agent-memory-server'
        self.wrapper.parent.mkdir(parents=True)
        self.write_tool(self.wrapper, FAKE_SERVER.format(events=self.events_file, runtime=self.runtime, real=BINARY,
                                                         state=self.state))
        script = SERVICE.parent / 'update-local.sh'
        if script.exists():
            shutil.copyfile(script, self.repo / 'update-local.sh')
            (self.repo / 'update-local.sh').chmod(0o755)
        python = sys.executable
        self.write_tool(self.fakebin / 'systemctl', FAKE_SYSTEMCTL.format(
            python=python, state=str(self.state), home=str(self.home), events=str(self.events_file),
            wrapper=str(self.wrapper)))
        for name, body in (('systemd-analyze', ''),
                           ('cargo', ''),
                           ('git', 'case "$1 $2" in "branch --show-current") echo master;; "rev-parse --short") echo abc1234;; esac\n'),
                           ('claude', ''),
                           ('codex', ''),
                           ('python3', 'exec %s "$@"\n' % python)):
            self.write_tool(self.fakebin / name, FAKE_TOOL.format(name=name, events=self.events_file, state=self.state,
                                                                  body=body))
        self.env = dict(os.environ, HOME=str(self.home), PATH=f'{self.fakebin}:{os.environ["PATH"]}')
        for name in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME'):
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
        (self.state / 'active').write_text('1')  # the previous version of the service is running ...
        (self.state / 'enabled').write_text('1')  # ... and enabled, so it starts at login

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

    def fail_tool(self, name, action=''):
        """Make a fake system tool exit 1 (systemctl: only for `action`, if given). It still logs the call."""
        (self.state / 'fail' / name).write_text(action)

    def crash_after_commit(self):
        """`agent-memory-server migrate` runs for real, commits, then exits non-zero."""
        (self.state / 'crash-after-commit').touch()

    def unit_enabled(self):
        return (self.state / 'enabled').exists()

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

    def fresh_config(self):
        """No config yet, as on a first install. The installer writes listen = 127.0.0.1:8787, which is the live
        service's port on this machine, so the sandbox's copy of the installer gets a private port instead."""
        for path in (self.home / '.config/agent-memory').iterdir():
            path.unlink()
        script = self.repo / 'service/install-user.py'
        text = script.read_text()
        self.test.assertIn('127.0.0.1:8787', text)
        script.write_text(text.replace('127.0.0.1:8787', f'127.0.0.1:{free_port()}'))
        self.test.assertNotIn('8787', script.read_text())

    def config(self):
        import tomllib
        return tomllib.loads((self.home / '.config/agent-memory/server.toml').read_text())

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

    def migrate_index(self):
        events = self.box.events()
        return events, next(i for i, e in enumerate(events) if e.startswith('binary migrate'))

    def assert_v1_stays_off(self, events, migrate):
        """After the commit nothing may start, restart or enable the unit: the old binary would serve a v2 store."""
        later = [e for e in events[migrate + 1:]
                 if e.startswith(('systemctl --user start', 'systemctl --user restart', 'systemctl --user enable'))]
        self.assertEqual(later, [])
        self.assertFalse(self.box.service_active())
        self.assertFalse(self.box.unit_enabled(), 'the next login must not start the old binary')

    def test_migrate_commits_then_fails_keeps_v1_off(self):
        v1_database(self.box.database, REVISIONS)
        self.box.crash_after_commit()
        ran = self.box.install('--migrate')
        self.assertNotEqual(ran.returncode, 0, ran.stdout)
        self.assertIn('rerun install-user.py or roll back', ran.stdout + ran.stderr)
        self.assertEqual(user_version(self.box.database), 2, 'the migration did commit')
        events, migrate = self.migrate_index()
        self.assertIn('systemctl --user disable agent-memory.service', events[:migrate])
        self.assert_v1_stays_off(events, migrate)

    def test_failure_after_migration_leaves_unit_disabled(self):
        v1_database(self.box.database, REVISIONS)
        self.box.fail_tool('systemd-analyze')
        ran = self.box.install('--migrate')
        self.assertNotEqual(ran.returncode, 0, ran.stdout)
        self.assertIn('rerun install-user.py or roll back', ran.stdout + ran.stderr)
        self.assertEqual(user_version(self.box.database), 2)
        events, migrate = self.migrate_index()
        self.assertIn('systemctl --user disable agent-memory.service', events[:migrate])
        self.assertTrue([e for e in events[migrate:] if e.startswith('systemd-analyze')], 'the failing step ran')
        self.assert_v1_stays_off(events, migrate)

    def test_failure_before_commit_restarts_v1(self):
        v1_database(self.box.database, OFFENDING)
        before = self.box.database.read_bytes()
        ran = self.box.install('--migrate')
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('machine learning', ran.stdout + ran.stderr, 'the offender is shown to the user')
        self.assertEqual(self.box.database.read_bytes(), before)
        self.assertEqual(self.box.installed(), [])
        events, migrate = self.migrate_index()
        self.assertIn('systemctl --user disable agent-memory.service', events[:migrate])
        self.assertTrue(self.box.service_active(), 'the stopped service is started again')
        self.assertTrue(self.box.unit_enabled(), 'and is enabled again, as it was found')


SESSION = {'name': 'agent-session', 'capabilities': ['recall', 'observe']}
MAINTAINER = ['recall', 'observe', 'review', 'promote']
V1_PROJECTS = '[projects]\nagent-memory = "/srv/projects/agent-memory"\n'


class TokenReconcileTests(unittest.TestCase):
    """Design §3.6: every install leaves an agent-session token and no agent-recall token or [projects]."""

    def setUp(self):
        self.box = Sandbox(self)
        self.box.migrated_database(REVISIONS)
        self.config_dir = self.box.home / '.config/agent-memory'

    def token(self, config, name):
        return next((t for t in config['tokens'] if t['name'] == name), None)

    def assert_reconciled(self, config):
        self.assertEqual(sorted(t['name'] for t in config['tokens']), ['agent-session', 'memory-maintainer'])
        self.assertEqual(self.token(config, 'agent-session')['capabilities'], ['recall', 'observe'])
        self.assertEqual(self.token(config, 'memory-maintainer')['capabilities'], MAINTAINER)
        self.assertNotIn('projects', config)
        client = json.loads((self.config_dir / 'client.json').read_text())
        self.assertEqual(hashlib.sha256(client['token'].encode()).hexdigest(),
                         self.token(config, 'agent-session')['sha256'], 'client.json holds the session token')

    def test_existing_config_gets_session_token(self):
        server = self.config_dir / 'server.toml'
        server.write_text(server.read_text().replace('[[tokens]]', V1_PROJECTS + '\n[[tokens]]', 1))
        old = {name: (self.config_dir / name).read_bytes() for name in ('server.toml', 'client.json', 'maintainer.json')}
        old_maintainer = self.box.config()['tokens'][1]
        self.assertEqual(old_maintainer['name'], 'memory-maintainer')
        self.assertIn('projects', self.box.config(), 'the fixture is v1-shaped')
        ran = self.box.install()
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        config = self.box.config()
        self.assert_reconciled(config)
        self.assertEqual(self.token(config, 'memory-maintainer'), old_maintainer, 'the maintainer token is untouched')
        self.assertEqual((self.config_dir / 'maintainer.json').read_bytes(), old['maintainer.json'])
        self.assertNotEqual((self.config_dir / 'client.json').read_bytes(), old['client.json'])
        # server.toml and client.json were copied to runtime/ first, private, byte for byte.
        saved = [path for path in self.box.runtime.iterdir() if path.is_file()]
        for name in ('server.toml', 'client.json'):
            copies = [path for path in saved if path.read_bytes() == old[name]]
            self.assertEqual(len(copies), 1, f'one backup of {name} among {saved}')
            self.assertEqual(copies[0].stat().st_mode & 0o777, 0o600)
        # A second run changes nothing: no new token, no new backup.
        before = {path.name: path.read_bytes() for path in self.config_dir.iterdir()}
        backups = sorted(path.name for path in self.box.runtime.iterdir())
        again = self.box.install()
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertEqual({path.name: path.read_bytes() for path in self.config_dir.iterdir()}, before)
        self.assertEqual(sorted(path.name for path in self.box.runtime.iterdir()), backups)

    def test_fresh_install_mints_session_token(self):
        self.box.fresh_config()
        ran = self.box.install()
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        config = self.box.config()
        self.assert_reconciled(config)
        maintainer = json.loads((self.config_dir / 'maintainer.json').read_text())
        self.assertEqual(hashlib.sha256(maintainer['token'].encode()).hexdigest(),
                         self.token(config, 'memory-maintainer')['sha256'])
        for name in ('server.toml', 'client.json', 'maintainer.json'):
            self.assertEqual((self.config_dir / name).stat().st_mode & 0o777, 0o600, name)
        self.assertTrue(self.box.service_active())

    def test_install_reads_the_database_from_server_toml(self):
        ran = self.box.run(sys.executable, str(self.box.repo / 'service/install-user.py'))
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertEqual(self.box.config()['database'], str(self.box.database))

    def test_first_install_needs_a_database(self):
        (self.config_dir / 'server.toml').unlink()
        ran = self.box.run(sys.executable, str(self.box.repo / 'service/install-user.py'))
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('First install', ran.stderr)

    def test_token_subcommand_removed(self):
        ran = subprocess.run([str(BINARY), 'token'], capture_output=True, text=True, timeout=30)
        self.assertNotEqual(ran.returncode, 0, ran.stdout)
        self.assertIn('unrecognized subcommand', ran.stderr)
        helped = subprocess.run([str(BINARY), '--help'], capture_output=True, text=True, timeout=30)
        self.assertIsNone(re.search(r'^\s+token\b', helped.stdout, re.M), helped.stdout)


class UpdateLocalTests(unittest.TestCase):
    def setUp(self):
        self.box = Sandbox(self)
        if not (self.box.repo / 'update-local.sh').exists():
            self.fail('update-local.sh does not exist')

    def installer_calls(self):
        return [e for e in self.box.events() if e.startswith('python3 ') and 'install-user.py' in e]

    def git(self, cwd, *args):
        subprocess.run(['git', '-c', 'user.name=t', '-c', 'user.email=t@example.com', *args], cwd=cwd, env=self.box.env,
                       check=True, capture_output=True, timeout=30)

    def repo_with_origin(self):
        """Swap the fake git for a real repository, for what only real git shows."""
        (self.box.fakebin / 'git').unlink()
        repo, origin = self.box.repo, self.box.repo.parent / 'origin.git'
        self.git(repo.parent, 'init', '--bare', '-b', 'master', str(origin))
        self.git(repo, 'init', '-b', 'master')
        self.git(repo, 'add', 'update-local.sh')
        self.git(repo, 'commit', '-m', 'first')
        self.git(repo, 'remote', 'add', 'origin', str(origin))
        self.git(repo, 'push', 'origin', 'master')
        return origin

    def test_update_local_refuses_a_linked_worktree(self):
        self.repo_with_origin()
        tree = self.box.repo.parent / 'worktree'
        self.git(self.box.repo, 'switch', '-c', 'feature')
        self.git(self.box.repo, 'worktree', 'add', str(tree), 'master')
        ran = subprocess.run(['./update-local.sh'], cwd=tree, env=self.box.env, capture_output=True, text=True, timeout=30)
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('worktree', ran.stderr)
        self.assertFalse([e for e in self.box.events() if e.startswith('cargo')])

    def test_update_local_reruns_itself_when_the_pull_replaces_it(self):
        self.box.migrated_database(REVISIONS)
        origin = self.repo_with_origin()
        other = self.box.repo.parent / 'other'
        self.git(self.box.repo.parent, 'clone', str(origin), str(other))
        script = other / 'update-local.sh'
        script.write_text(script.read_text().replace('set -eu\n', 'set -eu\necho the new script ran\n'))
        self.git(other, 'commit', '-am', 'second')
        self.git(other, 'push', 'origin', 'master')
        ran = self.box.run('./update-local.sh')
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertIn('the new script ran', ran.stdout)

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
        self.assertFalse((self.box.home / '.claude/hooks/agent-memory-recall.py').is_symlink(), 'nothing runs after the guard')
        self.assertEqual(self.box.database.read_bytes(), before)
        self.assertEqual(self.box.installed(), [])

    def test_update_local_links_the_skill_for_both_agents(self):
        self.box.migrated_database(REVISIONS)
        for _ in range(2):  # a rerun replaces the links in place
            ran = self.box.run('./update-local.sh')
            self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        for parent in ('.claude/skills', '.agents/skills'):
            link = self.box.home / parent / 'dream-memory'
            self.assertTrue(link.is_symlink(), link)
            self.assertEqual(os.readlink(link), str(self.box.repo / 'skill/dream-memory'))

    def test_update_local_refuses_a_skill_folder_that_is_not_a_link(self):
        self.box.migrated_database(REVISIONS)
        folder = self.box.home / '.agents/skills/dream-memory'
        folder.mkdir(parents=True)
        (folder / 'SKILL.md').write_text('a local copy\n')
        ran = self.box.run('./update-local.sh')
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('not a link', ran.stderr)
        self.assertEqual(sorted(p.name for p in folder.iterdir()), ['SKILL.md'], 'the copy is left alone')
        self.assertEqual(self.box.installed(), [], 'it stops before changing anything')

    def test_install_links_the_skill_when_given_the_database(self):
        # The script from before the skill links moved passes --database and has no link step of its own.
        self.box.migrated_database(REVISIONS)
        ran = self.box.install()
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        for parent in ('.claude/skills', '.agents/skills'):
            self.assertEqual(os.readlink(self.box.home / parent / 'dream-memory'), str(self.box.repo / 'skill/dream-memory'))

    def test_install_refuses_a_linked_worktree(self):
        self.box.migrated_database(REVISIONS)
        (self.box.repo / '.git').write_text('gitdir: elsewhere\n')
        ran = self.box.install()
        self.assertNotEqual(ran.returncode, 0)
        self.assertIn('worktree', ran.stderr)
        self.assertFalse((self.box.home / '.claude/skills/dream-memory').is_symlink())

    def test_install_clears_mcp_leftovers_once(self):
        self.box.migrated_database(REVISIONS)
        leftovers = [self.box.home / '.local/share/agent-memory-mcp', self.box.repo / 'service/.venv-mcp']
        for folder in leftovers:
            folder.mkdir(parents=True)
        (self.box.runtime).mkdir(exist_ok=True)
        (self.box.runtime / 'codex-config.toml').write_text('copy\n')
        calls = lambda: [e for e in self.box.events() if e.startswith(('claude ', 'codex '))]
        self.assertEqual(self.box.install().returncode, 0)
        self.assertEqual(calls(), ['claude mcp remove --scope user shared_memory', 'codex mcp remove shared_memory'])
        self.assertFalse([p for p in leftovers + [self.box.runtime / 'codex-config.toml'] if p.exists()])
        self.assertEqual(self.box.install().returncode, 0)
        self.assertEqual(len(calls()), 2, 'nothing left to clear')

    def test_install_carries_the_catalog_out_of_the_checkout_once(self):
        self.box.migrated_database(REVISIONS)
        old = self.box.repo / 'sources/latest-catalog.json'
        old.parent.mkdir()
        old.write_text('old catalog\n')
        self.box.env['XDG_STATE_HOME'] = str(self.box.home / 'custom-state')
        new = self.box.home / 'custom-state/agent-memory/sources/latest-catalog.json'
        self.assertEqual(self.box.install().returncode, 0)
        self.assertEqual(new.read_text(), 'old catalog\n')
        new.write_text('newer catalog\n')
        self.assertEqual(self.box.install().returncode, 0)
        self.assertEqual(new.read_text(), 'newer catalog\n')

    def test_update_local_finds_a_database_outside_the_checkout(self):
        self.box.migrated_database(REVISIONS)
        outside = self.box.repo.parent / 'data/memory.sqlite3'
        outside.parent.mkdir()
        self.box.database.rename(outside)
        server = self.box.home / '.config/agent-memory/server.toml'
        server.write_text(server.read_text().replace(str(self.box.database), str(outside)))
        ran = self.box.run('./update-local.sh')
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertEqual(self.box.config()['database'], str(outside))

    def test_update_local_updates_a_migrated_store(self):
        self.box.migrated_database(REVISIONS)
        ran = self.box.run('./update-local.sh')
        self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
        self.assertTrue(self.installer_calls())
        self.assertFalse([e for e in self.box.events() if '--migrate' in e or e.startswith('binary migrate')])
        self.assertEqual(user_version(self.box.database), 2)
        self.assertTrue(self.box.service_active())
        self.assertEqual(os.readlink(self.box.home / '.claude/hooks/agent-memory-recall.py'),
                         str(self.box.repo / 'hooks/agent-memory-recall.py'))


if __name__ == '__main__':
    unittest.main()
