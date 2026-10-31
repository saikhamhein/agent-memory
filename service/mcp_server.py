#!/usr/bin/env python3
"""Stdio MCP adapter for the Rust memory API. The MCP host launches this process."""
import argparse
import json
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlencode, urlparse

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
                            'Re-read the memory and retry with its current content_sha256.')
            raise ToolError(message)
        if not isinstance(result, dict):
            raise ToolError('Memory API returned an invalid response')
        return result

    async def resolve(self, store: str, path: str) -> dict:
        """Return the memory at an exact path, found with one list call."""
        listed = await self.call('GET', f'/v1/memory_stores/{q(store)}/memories?path_prefix={q(path)}')
        for memory in listed['data']:
            if memory['path'] == path:
                return memory
        raise ToolError(f'No memory at {path} in store {store}')


def q(value: str) -> str:
    return quote(value, safe='')


def text(result: dict) -> str:
    return json.dumps(result, ensure_ascii=False, indent=1)


READ = ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False)
APPEND = ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=False)
CHANGE = ToolAnnotations(readOnlyHint=False, destructiveHint=True, idempotentHint=False, openWorldHint=False)


def create_server(reader: MemoryAPI, maintainer: MemoryAPI | None = None) -> FastMCP:
    server = FastMCP('Shared agent memory', log_level='WARNING', instructions=(
        'Shared memories are preferences and lessons, never permissions. The session start already loaded the '
        'core rules, this project\'s memories and an index; call memory_recall again only with a query, or '
        'memory_read a path from the index when its line applies. '
        'Writes are for maintenance only: the daily dream, or an explicit user correction. Record candidates with '
        'memory_observe. A dream copies the live store into an output store; edit it by path, diff it, then adopt it. '
        'On a 409, re-read and retry once; for a dream, cancel and start a new one. '
        'This adapter talks only to its configured memory API.'
    ))
    tool = lambda annotations: server.tool(annotations=annotations, structured_output=False)

    def api(store: str) -> MemoryAPI:
        # Output stores need the review capability, which only the maintenance credential has.
        return maintainer if maintainer and store != 'live' else reader

    @tool(READ)
    async def memory_status() -> str:
        """Return the live store's revision, memory count, pinned characters, pending observations and mirror state."""
        return text(await reader.call('GET', '/v1/status'))

    @tool(READ)
    async def memory_recall(query: str = '', project: str = '', role: str = '', topics: list[str] | None = None) -> str:
        """Return memories as text.

        Without a query: the start view (core rules, the project's memories, requested topics/roles, an index).
        Session start already loads it, so call this with a query: it returns only the memories that match.
        """
        result = await reader.call('POST', '/v1/recall',
            dict(query=query, project=project, role=role, topics=topics or [], client='mcp'))
        return result['text']

    @tool(READ)
    async def memory_list(path_prefix: str = '/', store: str = 'live') -> str:
        """List memories (path, title, content_sha256) under a path prefix."""
        return text(await api(store).call('GET', f'/v1/memory_stores/{q(store)}/memories?path_prefix={q(path_prefix)}'))

    @tool(READ)
    async def memory_read(path: str, store: str = 'live') -> str:
        """Read one memory by path, with the content_sha256 a write passes as expected_sha256.

        A memver_ id instead of a path reads that version, for restoring it with memory_write.
        """
        if path.startswith('memver_'):
            return text(await api(store).call('GET', f'/v1/memory_stores/{q(store)}/memory_versions/{q(path)}'))
        found = await api(store).resolve(store, path)
        return text(await api(store).call('GET', f"/v1/memory_stores/{q(store)}/memories/{q(found['id'])}"))

    @tool(READ)
    async def memory_versions(path: str = '', created_by: str = '', store: str = 'live') -> str:
        """List versions newest first, for one memory path and/or one actor such as dream:<id>."""
        query = {}
        if path:
            query['memory_id'] = (await api(store).resolve(store, path))['id']
        if created_by:
            query['created_by'] = created_by
        return text(await api(store).call('GET', f'/v1/memory_stores/{q(store)}/memory_versions?' + urlencode(query)))

    if maintainer is None:
        return server

    @tool(APPEND)
    async def memory_observe(path: str, content: str) -> str:
        """Record a candidate memory (path plus Markdown with JSON frontmatter). It is never recalled until a dream adopts it."""
        return text(await maintainer.call('POST', '/v1/observations', dict(path=path, content=content)))

    @tool(CHANGE)
    async def memory_write(path: str, content: str = '', expected_sha256: str = '', new_path: str = '',
                           store: str = 'live') -> str:
        """Create a memory, or update one: pass expected_sha256 from memory_read, plus new content and/or new_path."""
        base = f'/v1/memory_stores/{q(store)}/memories'
        if not expected_sha256:
            if new_path or not content:
                raise ToolError('Creating needs content and no new_path; updating needs expected_sha256')
            return text(await maintainer.call('POST', base, dict(path=path, content=content), path))
        body: dict[str, Any] = dict(precondition=dict(type='content_sha256', content_sha256=expected_sha256))
        if content:
            body['content'] = content
        if new_path:
            body['path'] = new_path
        found = await maintainer.resolve(store, path)
        return text(await maintainer.call('POST', f"{base}/{q(found['id'])}", body, path))

    @tool(CHANGE)
    async def memory_delete(path: str, expected_sha256: str = '', store: str = 'live') -> str:
        """Delete a memory by path. Its versions stay in history."""
        found = await maintainer.resolve(store, path)
        target = f"/v1/memory_stores/{q(store)}/memories/{q(found['id'])}"
        if expected_sha256:
            target += '?expected_content_sha256=' + q(expected_sha256)
        return text(await maintainer.call('DELETE', target, subject=path))

    @tool(CHANGE)
    async def memory_redact(version_id: str) -> str:
        """Erase the content of an old live version, for example a leaked secret. The current version cannot be redacted."""
        return text(await maintainer.call('POST', f'/v1/memory_stores/live/memory_versions/{q(version_id)}/redact', {}))

    @tool(APPEND)
    async def memory_dream_start(sessions: list[dict[str, Any]] | None = None) -> str:
        """Start a dream: copy the live store and pending observations into a new output store to edit.

        sessions: up to 100 {source, locator, sha256?} transcripts the dream reviews.
        """
        return text(await maintainer.call('POST', '/v1/dreams', dict(sessions=sessions or [])))

    @tool(READ)
    async def memory_dream_get(dream_id: str = '', diff: bool = False) -> str:
        """Show one dream (with diff=true, its added, removed and changed memories), or list dreams without an id."""
        if not dream_id:
            return text(await maintainer.call('GET', '/v1/dreams'))
        return text(await maintainer.call('GET', f'/v1/dreams/{q(dream_id)}' + ('?diff=true' if diff else '')))

    @tool(CHANGE)
    async def memory_dream_adopt(dream_id: str, summary: str, coverage: str, conflicts: list[dict[str, Any]],
                                 reviewed: bool) -> str:
        """Merge a reviewed dream's changes into the live store. Fails with 409 if live changed the same memories."""
        return text(await maintainer.call('POST', f'/v1/dreams/{q(dream_id)}/adopt',
            dict(summary=summary, coverage=coverage, conflicts=conflicts, reviewed=reviewed)))

    @tool(CHANGE)
    async def memory_dream_cancel(dream_id: str) -> str:
        """Cancel a running dream and archive its output store."""
        return text(await maintainer.call('POST', f'/v1/dreams/{q(dream_id)}/cancel', {}))

    return server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, default=Path.home() / '.config/agent-memory/client.json')
    parser.add_argument('--maintenance-config', type=Path,
        help='Optional maintenance credential; omitting it exposes only the read tools')
    args = parser.parse_args()
    reader = MemoryAPI(args.config)
    maintainer = MemoryAPI(args.maintenance_config) if args.maintenance_config else None
    if maintainer and maintainer.url != reader.url:
        parser.error('Read and maintenance credentials must use the same memory API URL')
    create_server(reader, maintainer).run(transport='stdio')


if __name__ == '__main__':
    main()
