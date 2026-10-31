#!/usr/bin/env python3
"""Old loader entry point: `memory.py recall …` runs `agent-memory-client recall …` (service/client.py).

It keeps the `python3 …/memory.py recall --query …` lines in Codex's and DeepSeek's AGENTS.md working.
Those lines always pass a query, so it prints the start view, a blank line, then the matches.
The old global --root is ignored. Every other command now belongs to agent-memory-client."""
import argparse
from pathlib import Path
import subprocess
import sys

options = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
options.add_argument('--root')
options.add_argument('--query', default='')
known, rest = options.parse_known_args()
if rest[:1] != ['recall']:
    print('memory.py only forwards recall now. Use agent-memory-client (service/client.py); see README.md.',
          file=sys.stderr)
    sys.exit(2)
recall = [sys.executable, str(Path(__file__).resolve().parent / 'service/client.py'), *rest]
code = subprocess.run(recall).returncode
if known.query and code == 0:
    print(flush=True)
    code = subprocess.run([*recall, '--query', known.query]).returncode
sys.exit(code)
