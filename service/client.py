#!/usr/bin/env python3
"""Small HTTP client for the shared memory service; Python standard library only."""
import argparse
import json
from pathlib import Path
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # Never forward an API credential to a redirected destination.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=Path.home() / '.config/agent-memory/client.json')
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('status')
    commands.add_parser('export')
    recall = commands.add_parser('recall')
    project = recall.add_mutually_exclusive_group()
    project.add_argument('--project', default='')
    project.add_argument('--project-id', default='')
    recall.add_argument('--query', default='')
    recall.add_argument('--role', default='')
    recall.add_argument('--topic', action='append', default=[])
    recall.add_argument('--limit', type=int, default=12)
    recall.add_argument('--budget', type=int, default=12000)
    recall.add_argument('--json', action='store_true')
    for name in ('observe', 'validate', 'diff', 'promote'):
        command = commands.add_parser(name)
        command.add_argument('file', type=Path)
    dream = commands.add_parser('dream')
    dream.add_argument('--output', type=Path, help='New local directory for input.json and proposal.json')
    get_dream = commands.add_parser('get-dream')
    get_dream.add_argument('id')
    rollback = commands.add_parser('rollback')
    rollback.add_argument('revision', type=int)
    rollback.add_argument('--expected', required=True, type=int)
    rollback.add_argument('--reason', required=True)
    args = parser.parse_args()
    config = json.loads(args.config.read_text())
    url = config['url'].rstrip('/')
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('Configure an http(s) server URL without credentials, query, or fragment')
    body = None
    if args.command == 'recall':
        path = '/v1/recall'
        body = dict(project=args.project, project_id=args.project_id, role=args.role,
                    topics=args.topic, query=args.query, limit=args.limit, budget=args.budget)
    elif args.command in ('observe', 'validate', 'diff', 'promote'):
        path = '/v1/observations' if args.command == 'observe' else '/v1/proposals/' + args.command
        body = json.loads(args.file.read_text())
    elif args.command == 'dream':
        if args.output and args.output.exists():
            raise ValueError('Dream output directory already exists; choose a new directory')
        path = '/v1/dreams'
        body = {}
    elif args.command == 'get-dream':
        path = '/v1/dreams/' + urllib.parse.quote(args.id, safe='')
    elif args.command == 'rollback':
        path = '/v1/rollback'
        body = dict(revision=args.revision, expected=args.expected, reason=args.reason)
    else:
        path = '/v1/' + args.command
    request = urllib.request.Request(url + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + config['token'], 'Content-Type': 'application/json'})
    # Avoid sending local memory or credentials through a configured HTTP proxy.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(request, timeout=30) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        message = error.read().decode(errors='replace')
        raise ValueError(f'Memory API returned HTTP {error.code}: {message}') from None
    if args.command == 'recall' and not args.json:
        print(result['text'], end='')
    elif args.command == 'dream' and args.output:
        args.output.mkdir(parents=True, mode=0o700)
        for name in ('input', 'proposal'):
            target = args.output / (name + '.json')
            with target.open('x') as output:
                output.write(json.dumps(result[name], ensure_ascii=False, indent=2) + '\n')
            target.chmod(0o600)
        print(args.output / 'proposal.json')
    else:
        print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, urllib.error.URLError) as error:
        print(f'Error: {error}', file=sys.stderr)
        sys.exit(1)
