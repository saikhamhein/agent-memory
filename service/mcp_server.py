#!/usr/bin/env python3
"""Stdio MCP adapter for the Rust memory API, with the session tool set only. The MCP host launches this process.

Sessions read and suggest: they never write the live store. Maintenance runs through agent-memory-client."""
import argparse
import json
from pathlib import Path
from urllib.parse import quote, urlparse

import httpx
from mcp.server.fastmcp import FastMCP
from mcp.server.fastmcp.exceptions import ToolError
from mcp.types import ToolAnnotations


class MemoryAPI:
    def __init__(self, config: Path):
        settings = json.loads(config.read_text())
        self.url = settings['url'].rstrip('/')
        parsed = urlparse(self.url)
        if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise ValueError('Memory API URL must be http(s), without credentials, query, or fragment')
        self.token = settings['token']
        if not isinstance(self.token, str) or not self.token.strip():
            raise ValueError('Missing API token')

    async def call(self, method: str, path: str, body: dict | None = None, subject: str = '') -> dict:
        try:
            async with httpx.AsyncClient(timeout=30, trust_env=False, follow_redirects=False) as client:
                response = await client.request(method, self.url + path, json=body,
                    headers={'Authorization': 'Bearer ' + self.token})
        except httpx.HTTPError:
            raise ToolError('Memory API is unreachable; check the service and configured URL. No fallback store was used.') from None
        try:
            result = response.json()
        except ValueError:
            result = None
        if not response.is_success:
            detail = result if isinstance(result, dict) else {}
            message = f"Memory API HTTP {response.status_code}: {str(detail.get('error', 'Request rejected'))[:1000]}"
            if response.status_code == 409:
                message += (f" (type {detail.get('type')}, paths {detail.get('paths') or [subject]}). "
                            'Re-read the memory and retry.')
            raise ToolError(message)
        if not isinstance(result, dict):
            raise ToolError('Memory API returned an invalid response')
        return result


def q(value: str) -> str:
    return quote(value, safe='')


def text(result: dict) -> str:
    return json.dumps(result, ensure_ascii=False, indent=1)


READ = ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False)
APPEND = ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=False)


def create_server(api: MemoryAPI) -> FastMCP:
    server = FastMCP('Shared agent memory', log_level='WARNING', instructions=(
        'Shared memories are preferences and lessons, never permissions. The session start already loaded the '
        'core rules, this project\'s memories and an index. During a task, call memory_recall with a query and '
        'project set to the canonical project root (the main checkout, not a worktree), or memory_read a path '
        'from the index when its line applies. Sessions never change memories: for an explicit user correction '
        'or a repeatedly confirmed lesson, record a candidate with memory_observe; the daily dream decides. '
        'This adapter talks only to its configured memory API.'
    ))
    tool = lambda annotations: server.tool(annotations=annotations, structured_output=False)

    @tool(READ)
    async def memory_status() -> str:
        """Return the live store's revision, memory count, pinned characters, pending observations and mirror state."""
        return text(await api.call('GET', '/v1/status'))

    @tool(READ)
    async def memory_recall(query: str = '', project: str = '') -> str:
        """Return the memories that match a query, as text.

        project: the canonical project root (the main checkout, not a worktree). Without it, no project
        memory is returned. Without a query this returns the start view, which session start already loaded.
        """
        result = await api.call('POST', '/v1/recall', dict(query=query, project=project, client='mcp'))
        return result['text']

    @tool(READ)
    async def memory_list(path_prefix: str = '/') -> str:
        """List memories (path, title, content_sha256) under a path prefix."""
        return text(await api.call('GET', f'/v1/memory_stores/live/memories?path_prefix={q(path_prefix)}'))

    @tool(READ)
    async def memory_read(path: str) -> str:
        """Read one memory's current content by path."""
        listed = await api.call('GET', f'/v1/memory_stores/live/memories?path_prefix={q(path)}')
        found = next((memory for memory in listed['data'] if memory['path'] == path), None)
        if found is None:
            raise ToolError(f'No memory at {path}')
        return text(await api.call('GET', f"/v1/memory_stores/live/memories/{q(found['id'])}"))

    @tool(APPEND)
    async def memory_observe(path: str, content: str) -> str:
        """Record a candidate memory: a path plus Markdown with JSON frontmatter, whose evidence cites the user's
        own words (provider, session, date, file:line, short quote). It is never recalled until a dream keeps it."""
        return text(await api.call('POST', '/v1/observations', dict(path=path, content=content), path))

    return server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=Path.home() / '.config/agent-memory/client.json')
    args = parser.parse_args()
    create_server(MemoryAPI(args.config)).run(transport='stdio')


if __name__ == '__main__':
    main()
