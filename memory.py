#!/usr/bin/env python3
"""Old loader entry point: `memory.py recall …` runs `agent-memory-client recall …` (service/client.py).

It keeps the existing `python3 …/memory.py recall …` lines in Codex's and DeepSeek's AGENTS.md working.
Every other command now belongs to agent-memory-client."""
from pathlib import Path
import subprocess
import sys

if sys.argv[1:2] != ['recall']:
    print('memory.py only forwards recall now. Use agent-memory-client (service/client.py); see README.md.',
          file=sys.stderr)
    sys.exit(2)
client = Path(__file__).resolve().parent / 'service/client.py'
sys.exit(subprocess.run([sys.executable, str(client), *sys.argv[1:]]).returncode)
