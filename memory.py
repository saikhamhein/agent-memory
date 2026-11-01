#!/usr/bin/env python3
"""Old loader entry point: `memory.py recall …` runs `agent-memory-client recall …` (service/client.py).

It keeps the `python3 …/memory.py recall --query …` lines in Codex's and DeepSeek's AGENTS.md working.
Those lines always pass a query, so it prints the start view, then the matches it didn't already show.
The old global --root is ignored. Every other command now belongs to agent-memory-client."""
import argparse
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent / 'service'))
import client  # noqa: E402

options = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
options.add_argument('--root')
rest = options.parse_known_args()[1]
if rest[:1] != ['recall']:
    print('memory.py only forwards recall now. Use agent-memory-client (service/client.py); see README.md.',
          file=sys.stderr)
    sys.exit(2)
args = client.parser().parse_args(rest)
try:
    api = client.API(args.config)
    start = client.recall(api, args, '')
    sys.stdout.write(start['text'])
    if args.query:
        task = client.recall(api, args, args.query)
        shown, heads = ('|'.join(map(re.escape, paths)) or '(?!)' for paths in (start['returned'], task['returned']))
        # Drop each full block, and each "More matches" line, of a memory the start view already showed.
        text = re.sub(rf'(?ms)^### [^\n]* \[(?:{shown})\]\n.*?(?=^### [^\n]* \[(?:{heads})\]$|^## More matches$|\Z)'
                      rf'|^- (?:{shown}) — [^\n]*\n', '', task['text'])
        text = re.sub(r'\n## More matches\s*\Z', '', text).rstrip() + '\n'
        if text == task['text'] or text.partition('\n\n')[2].strip():
            sys.stdout.write('\n' + text)
except client.ERRORS as error:
    print(f'Error: {error}', file=sys.stderr)
    sys.exit(1)
