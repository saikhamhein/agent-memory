"""Shared fixture for the Python suites: the real release server on a temporary store.

Safe by construction: every path lives in one temporary directory, the listener is
127.0.0.1:0, and `home()` gives clients a throwaway HOME, so nothing under ~ is read.
Set AGENT_MEMORY_BINARY to test another build of agent-memory-server.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request

SERVICE = Path(__file__).resolve().parents[1]
REPO = SERVICE.parent
BINARY = Path(os.environ.get('AGENT_MEMORY_BINARY') or SERVICE / 'target/release/agent-memory-server')

# role -> capabilities (design §4.1: reads need recall, writes to the live store need promote)
ROLES = {
    'agent-session': ['recall', 'observe'],  # client.json: the hook, loaders, MCP and the client's default
    'reader': ['recall'],
    'observer': ['observe'],
    'reviewer': ['recall', 'review'],
    'admin': ['recall', 'observe', 'review', 'promote'],  # maintainer.json: the daily dream only
}
NO_PROXY = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def sha(data):
    return hashlib.sha256(data if isinstance(data, bytes) else data.encode()).hexdigest()


def content(title, body='Check existing evidence before asking.', **meta):
    """Valid memory file text (§3.1 format). Extra keyword arguments become frontmatter fields."""
    front = {'title': title, 'confidence': 'explicit', 'tags': ['fixture'],
             'evidence': [{'source': 'test', 'locator': 'fixture:1', 'note': 'Fixture only'}]}
    front.update(meta)
    return '---\n' + json.dumps(front, ensure_ascii=False) + '\n---\n' + body


class Server:
    """A running server. Use `with Server() as s` or `start_server(testcase)`."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.process = None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.close()

    @property
    def mirror(self):
        return self.root / 'mnt' / 'shared-agent-memory'

    @property
    def recall_log(self):
        return self.root / 'recall-log.jsonl'

    def start(self):
        database = self.root / 'memory.sqlite3'
        subprocess.run([str(BINARY), 'init', '--database', str(database)], check=True, capture_output=True)
        tokens = ''.join(
            f'[[tokens]]\nname = "{role}"\nsha256 = "{sha("fixture-" + role)}"\n'
            f'capabilities = {json.dumps(capabilities)}\n' for role, capabilities in ROLES.items())
        (self.root / 'server.toml').write_text(
            f'listen = "127.0.0.1:0"\ndatabase = {json.dumps(str(database))}\n' + tokens)
        self.process = subprocess.Popen([str(BINARY), 'serve', '--config', str(self.root / 'server.toml')],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        line = self.process.stderr.readline()
        if 'listening on ' not in line:
            raise RuntimeError('server did not start: ' + line)
        self.url = 'http://' + line.strip().split('listening on ')[1]
        for role in ROLES:
            (self.root / (role + '.json')).write_text(json.dumps({'url': self.url, 'token': 'fixture-' + role}))
        for _ in range(50):
            try:
                with NO_PROXY.open(self.url + '/healthz', timeout=1):
                    return
            except OSError:
                time.sleep(0.1)
        raise RuntimeError('HTTP server did not become ready')

    def stop(self):
        """Stop the server but keep the store and mirror on disk."""
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise RuntimeError('SIGTERM did not shut down the server')
        if self.process and self.process.stderr:
            self.process.stderr.close()

    def close(self):
        self.stop()
        for directory, names, _ in os.walk(self.root):  # mirror directories are read-only (0555)
            for name in names:
                os.chmod(os.path.join(directory, name), 0o755)
        self.tmp.cleanup()

    def config(self, role):
        return self.root / (role + '.json')

    def api(self, method, path, body=None, role='admin'):
        """Raw HTTP call. Returns (status, parsed JSON or None)."""
        request = urllib.request.Request(self.url + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer fixture-' + role, 'Content-Type': 'application/json'})
        try:
            with NO_PROXY.open(request, timeout=10) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            raw = error.read()
            try:
                return error.code, json.loads(raw)
            except ValueError:
                return error.code, None

    def seed(self, path, title=None, body='Check existing evidence before asking.', **meta):
        """Create a live memory as admin; returns the memory object."""
        status, memory = self.api('POST', '/v1/memory_stores/live/memories', dict(path=path,
            content=content(title or path.rsplit('/', 1)[-1][:-3], body, **meta)))
        if status != 200:
            raise AssertionError(f'seed {path}: {status} {memory}')
        return memory

    def recall_entries(self):
        if not self.recall_log.exists():
            return []
        return [json.loads(line) for line in self.recall_log.read_text().splitlines()]

    def home(self):
        """A throwaway HOME whose default client config and server.toml point at this server.

        Clients (client.py, memory.py, the hook) find their config at their default
        locations under HOME, so tests never read the real ~/.config/agent-memory.
        Returns (home_path, env).
        """
        home = self.root / 'home'
        config = home / '.config' / 'agent-memory'
        config.mkdir(parents=True, exist_ok=True)
        shutil.copy(self.config('agent-session'), config / 'client.json')
        shutil.copy(self.config('admin'), config / 'maintainer.json')
        shutil.copy(self.root / 'server.toml', config / 'server.toml')
        env = {k: v for k, v in os.environ.items() if not k.startswith('AGENT_MEMORY')}
        env['HOME'] = str(home)
        return home, env

    def client(self, *args, role='admin', env=None, binary=False, check=None):
        """Run service/client.py with this server's credential for `role`."""
        config = [] if role is None else ['--config', str(self.config(role))]  # None: the client's default config
        result = subprocess.run([sys.executable, str(SERVICE / 'client.py'), *config, *map(str, args)],
                                capture_output=True, text=not binary, timeout=20, env=env)
        if check is True and result.returncode != 0:
            raise AssertionError(f'client {args}: {result.returncode} {result.stderr}')
        return result


def repo_with_worktree(base: Path, env):
    """A git checkout with a commit and a linked worktree beside it; returns (checkout, worktree), resolved."""
    git = ['git', '-c', 'user.name=t', '-c', 'user.email=t@t']
    repo = (base / 'checkout').resolve()
    worktree = repo.parent / 'worktree'
    subprocess.run([*git, 'init', '-q', str(repo)], check=True, env=env)
    subprocess.run([*git, '-C', str(repo), 'commit', '-q', '--allow-empty', '-m', 'x'], check=True, env=env)
    subprocess.run([*git, '-C', str(repo), 'worktree', 'add', '-q', str(worktree)], check=True, env=env)
    return repo, worktree


def project_path(root, slug='rule'):
    """The memory path whose scope is the project root `root` (§3.1)."""
    return '/projects/' + '/'.join(Path(root).parts[1:]) + f'/{slug}.md'


def start_server(case: unittest.TestCase) -> Server:
    server = Server()
    case.addCleanup(server.close)
    server.start()
    return server
