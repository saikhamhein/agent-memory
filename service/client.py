#!/usr/bin/env python3
"""Command-line client for the shared memory service; Python standard library only."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

CONFIG = Path.home() / '.config/agent-memory/client.json'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # Never forward an API credential to a redirected destination.


class API:
    def __init__(self, config: Path):
        settings = json.loads(config.read_text())
        self.url = settings['url'].rstrip('/')
        parsed = urllib.parse.urlparse(self.url)
        if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise ValueError('Configure an http(s) server URL without credentials, query, or fragment')
        self.token = settings['token']
        # Avoid sending local memory or credentials through a configured HTTP proxy.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def call(self, method, path, body=None):
        request = urllib.request.Request(self.url + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json'})
        try:
            with self.opener.open(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            message = error.read().decode(errors='replace')
            raise ValueError(f'Memory API returned HTTP {error.code}: {message}') from None

    def resolve(self, store, path):
        """Return the memory at an exact path, found with one list call."""
        listed = self.call('GET', f'/v1/memory_stores/{q(store)}/memories?path_prefix={q(path)}')
        for memory in listed['data']:
            if memory['path'] == path:
                return memory
        raise ValueError(f'No memory at {path} in store {store}')


def q(value):
    return urllib.parse.quote(value, safe='')


def canonical_root(path):
    """Map a directory in a Git worktree to the same place in the main checkout; other paths are kept."""
    try:
        found = subprocess.run(['git', '-C', path, 'rev-parse', '--path-format=absolute', '--show-toplevel',
                                '--git-common-dir'], capture_output=True, text=True, timeout=5).stdout.split('\n')
    except (OSError, subprocess.SubprocessError):
        return path
    if len(found) < 2 or not found[1].endswith('/.git'):
        return path
    try:
        inside = Path(path).resolve().relative_to(Path(found[0]).resolve())
    except ValueError:
        return path
    return str(Path(found[1][:-len('/.git')]) / inside)


def remote_key(path):
    """The project as `/<host>/<owner>/<repo>[/<subdir>]` from the origin remote, or '' without one.

    Any checkout of the repository, on any machine and whichever URL form it cloned with, gets the same key."""
    try:
        url, prefix = [subprocess.run(['git', '-C', path, *command], capture_output=True, text=True, timeout=5).stdout.strip()
                       for command in (['remote', 'get-url', 'origin'], ['rev-parse', '--show-prefix'])]
    except (OSError, subprocess.SubprocessError):
        return ''
    found = re.match(r'^[a-z][a-z0-9+.-]*://(?:[^@/]*@)?([^/:@]+)(?::\d+)?/(.+)$', url, re.I)
    if not found and '://' not in url:
        found = re.match(r'^(?:[^@/:]+@)?([^:/@]+):(?!//)(.+)$', url)  # git@host:owner/repo
    if not found:
        return ''
    repo = found.group(2).strip('/')
    key = '/'.join([found.group(1), repo.removesuffix('.git') if repo.endswith('.git') else repo, *prefix.strip('/').split('/')]).lower()
    segments = [s for s in key.split('/') if s]
    if not all(re.fullmatch(r'[a-z0-9._-]+', s) and s not in ('.', '..') for s in segments):
        return ''
    return '/' + '/'.join(segments)


def project_directory(path):
    """The directory under /projects/ that holds a project's memories: its git remote's key, else its path."""
    key = remote_key(path)
    return '/projects' + (key or canonical_root(path))


def parser():
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument('--config', type=Path, default=CONFIG)
    commands = root.add_subparsers(dest='command', required=True)
    commands.add_parser('status')
    project = commands.add_parser('project', help='print the /projects/... directory that holds a directory\'s memories')
    project.add_argument('directory', nargs='?', default='.', type=Path)
    recall = commands.add_parser('recall', help='Start view without --query; matching memories with it')
    recall.add_argument('--project', default='', help='Project directory; a worktree maps to its main checkout')
    recall.add_argument('--query', default='')
    recall.add_argument('--role', default='')
    recall.add_argument('--topic', action='append', default=[])
    recall.add_argument('--client', default='')
    for ignored in ('--limit', '--budget'):
        recall.add_argument(ignored, help=argparse.SUPPRESS)  # Accepted from old loader lines; ignored.
    recall.add_argument('--json', action='store_true', help=argparse.SUPPRESS)
    ls = commands.add_parser('ls')
    ls.add_argument('prefix', nargs='?', default='/')
    cat = commands.add_parser('cat', help='Print a memory, or a memver_ version, as its exact bytes')
    cat.add_argument('path')
    versions = commands.add_parser('versions')
    versions.add_argument('path', nargs='?')
    versions.add_argument('--created-by')
    observe = commands.add_parser('observe')
    observe.add_argument('path')
    observe.add_argument('file', help='A memory file, or - for stdin')
    useful = commands.add_parser('useful', help='Mark a recalled memory that helped; the dream weighs it')
    useful.add_argument('path')
    useful.add_argument('--why', required=True, help='How it changed what you did')
    write = commands.add_parser('write', help='Create, or update with --expected')
    write.add_argument('path')
    write.add_argument('file', type=Path)
    write.add_argument('--expected', help='content_sha256 being replaced')
    write.add_argument('--to', help='New path')
    rm = commands.add_parser('rm')
    rm.add_argument('path')
    rm.add_argument('--expected')
    redact = commands.add_parser('redact', help='Erase an old memory version (memver_…) or an observation')
    redact.add_argument('id')
    for command in (ls, cat, versions, write, rm):
        command.add_argument('--store', default='live')
    dream = commands.add_parser('dream').add_subparsers(dest='action', required=True)
    dream.add_parser('start').add_argument('--sessions', type=Path, help='JSON list of {source, locator, sha256?}')
    show = dream.add_parser('show')
    show.add_argument('id', nargs='?')
    show.add_argument('--diff', action='store_true')
    adopt = dream.add_parser('adopt')
    adopt.add_argument('id')
    adopt.add_argument('review', type=Path, help='JSON {summary, coverage, conflicts}')
    dream.add_parser('cancel').add_argument('id')
    return root


def recall(api, args, query):
    project = canonical_root(args.project) if args.project else ''
    remote = remote_key(args.project) if args.project else ''
    return api.call('POST', '/v1/recall', dict(project=project, remote=remote, role=args.role, topics=args.topic,
        query=query, client=args.client))


def run(args):
    if args.command == 'project':
        sys.stdout.write(project_directory(str(args.directory.resolve())) + '\n')
        return None
    api = API(args.config)
    store = q(getattr(args, 'store', 'live'))
    memories = f'/v1/memory_stores/{store}/memories'
    if args.command == 'status':
        return api.call('GET', '/v1/status')
    if args.command == 'recall':
        sys.stdout.write(recall(api, args, args.query)['text'])
        return None
    if args.command == 'ls':
        return api.call('GET', f'{memories}?path_prefix={q(args.prefix)}')
    if args.command == 'cat':
        if args.path.startswith('memver_'):
            content = api.call('GET', f'/v1/memory_stores/{store}/memory_versions/{q(args.path)}')['content']
        else:
            content = api.call('GET', f"{memories}/{q(api.resolve(args.store, args.path)['id'])}")['content']
        sys.stdout.buffer.write(content.encode())
        return None
    if args.command == 'versions':
        query = {}
        if args.path:
            query['memory_id'] = api.resolve(args.store, args.path)['id']
        if args.created_by:
            query['created_by'] = args.created_by
        return api.call('GET', f'/v1/memory_stores/{store}/memory_versions?' + urllib.parse.urlencode(query))
    if args.command == 'observe':
        data = sys.stdin.buffer.read() if args.file == '-' else Path(args.file).read_bytes()
        return api.call('POST', '/v1/observations', dict(path=args.path, content=data.decode()))
    if args.command == 'useful':
        return api.call('POST', '/v1/feedback', dict(path=args.path, why=args.why))
    if args.command == 'write':
        content = args.file.read_bytes().decode()
        if args.expected is None:
            if args.to:
                raise ValueError('--to renames an existing memory and needs --expected')
            return api.call('POST', memories, dict(path=args.path, content=content))
        body = dict(content=content, precondition=dict(type='content_sha256', content_sha256=args.expected))
        if args.to:
            body['path'] = args.to
        return api.call('POST', f"{memories}/{q(api.resolve(args.store, args.path)['id'])}", body)
    if args.command == 'rm':
        target = f"{memories}/{q(api.resolve(args.store, args.path)['id'])}"
        if args.expected:
            target += '?expected_content_sha256=' + q(args.expected)
        return api.call('DELETE', target)
    if args.command == 'redact':
        if args.id.startswith('memver_'):
            return api.call('POST', f'/v1/memory_stores/live/memory_versions/{q(args.id)}/redact', {})
        return api.call('POST', f'/v1/observations/{q(args.id)}/redact', {})
    if args.action == 'start':
        return api.call('POST', '/v1/dreams', dict(sessions=json.loads(args.sessions.read_text()) if args.sessions else []))
    if args.action == 'show':
        if not args.id:
            return api.call('GET', '/v1/dreams')
        return api.call('GET', f'/v1/dreams/{q(args.id)}' + ('?diff=true' if args.diff else ''))
    if args.action == 'adopt':
        return api.call('POST', f'/v1/dreams/{q(args.id)}/adopt', json.loads(args.review.read_text()))
    return api.call('POST', f'/v1/dreams/{q(args.id)}/cancel', {})


ERRORS = (OSError, ValueError, KeyError, urllib.error.URLError)


def main():
    try:
        result = run(parser().parse_args())
    except ERRORS as error:
        print(f'Error: {error}', file=sys.stderr)
        sys.exit(1)
    if result is not None:
        print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
