"""Real HTTP/process tests against an isolated temporary database, never the live store."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request

SERVICE = Path(__file__).resolve().parents[1]
BINARY = SERVICE / 'target/release/agent-memory-server'


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        database = self.root / 'memory.sqlite3'
        subprocess.run([str(BINARY), 'init', '--database', str(database)], check=True, capture_output=True)
        server = self.root / 'server.toml'
        server.write_text(f'listen = "127.0.0.1:0"\ndatabase = {json.dumps(str(database))}\n'
            '[[tokens]]\nname = "maintainer"\n'
            f'sha256 = "{hashlib.sha256(b"fixture-maintainer").hexdigest()}"\n'
            'capabilities = ["recall", "observe", "review", "promote"]\n'
            '[[tokens]]\nname = "reader"\n'
            f'sha256 = "{hashlib.sha256(b"fixture-reader").hexdigest()}"\n'
            'capabilities = ["recall"]\n')
        self.process = subprocess.Popen([str(BINARY), 'serve', '--config', str(server)],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop_server)
        line = self.process.stderr.readline()
        self.assertIn('listening on ', line)
        self.url = 'http://' + line.strip().split('listening on ')[1]
        for name, token in [('maintainer', 'fixture-maintainer'), ('reader', 'fixture-reader')]:
            (self.root / (name + '.json')).write_text(json.dumps({'url': self.url, 'token': token}))
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for _ in range(50):
            try:
                with opener.open(self.url + '/healthz', timeout=1) as response:
                    self.assertEqual(json.load(response), {'status': 'ok'})
                break
            except OSError:
                time.sleep(0.1)
        else:
            self.fail('HTTP server did not become ready')

    def stop_server(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                self.fail('SIGTERM did not shut down the server')
        self.process.stderr.close()

    def client(self, *args, role='maintainer', success=True):
        result = subprocess.run([sys.executable, str(SERVICE / 'client.py'), '--config',
            str(self.root / (role + '.json')), *map(str, args)], capture_output=True, text=True, timeout=10)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_client_review_workflow(self):
        candidate = {'id': 'example', 'title': 'Example', 'body': 'Check existing evidence.',
            'scope': {'kind': 'global'}, 'confidence': 'explicit', 'tags': ['evidence'],
            'evidence': [{'source': 'test', 'locator': 'fixture:1', 'note': 'Fixture only',
                'numbers': [18446744073709551617, -(2**130), 1e-7, 1e20, -0.0]}]}
        file = self.root / 'candidate.json'
        file.write_text(json.dumps(candidate))
        denied = self.client('observe', file, role='reader', success=False)
        self.assertIn('403', denied.stderr)
        self.client('observe', file)
        output = self.root / 'review'
        self.client('dream', '--output', output)
        original = (output / 'input.json').read_bytes()
        file = output / 'proposal.json'
        proposal = json.loads(file.read_text())
        proposal.update(memories=[candidate], reviewed=True, summary='Fixture', coverage='Fixture only')
        file.write_text(json.dumps(proposal))
        self.client('validate', file)
        self.assertEqual(json.loads(self.client('diff', file).stdout)['added'], [candidate])
        self.assertEqual(json.loads(self.client('promote', file).stdout)['revision'], 1)
        self.assertEqual((output / 'input.json').read_bytes(), original)
        recalled = json.loads(self.client('recall', '--json', role='reader').stdout)
        self.assertEqual(recalled['memories'], [candidate])
        self.assertIn('Check existing evidence.', self.client('recall', role='reader').stdout)
        self.assertEqual(json.loads(self.client('export').stdout)['revision'], 1)
        self.assertEqual(json.loads(self.client('get-dream', proposal['dream_id']).stdout)['proposal'], proposal)
        self.assertIn('409', self.client('promote', file, success=False).stderr)
        self.client('rollback', '0', '--expected', '1', '--reason', 'Fixture restore')
        self.assertEqual(json.loads(self.client('status', role='reader').stdout)['revision'], 2)

    def test_client_reports_service_failure(self):
        self.stop_server()
        result = self.client('recall', role='reader', success=False)
        self.assertIn('Error:', result.stderr)
        self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
