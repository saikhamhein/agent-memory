"""Official SDK client -> stdio adapter -> real Rust HTTP server (design §4.3 C), on a scratch store.

The adapter is the session tool set only: ordinary sessions read and suggest (X1). Memory writes and
dreams are tested through the client CLI in test_client.py.
"""
from contextlib import asynccontextmanager
import json
import subprocess
import sys
import unittest

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.types import TextContent
from service.tests.harness import SERVICE, content, project_path, sha, start_server

SESSION_TOOLS = {'memory_status', 'memory_recall', 'memory_list', 'memory_read', 'memory_observe', 'memory_useful'}
REMOVED_TOOLS = {'memory_versions', 'memory_write', 'memory_delete', 'memory_redact', 'memory_dream_start',
    'memory_dream_get', 'memory_dream_adopt', 'memory_dream_cancel', 'memory_dream', 'memory_get_dream',
    'memory_validate', 'memory_diff', 'memory_promote', 'memory_rollback', 'memory_export'}
START_HEADING = '# Shared agent memory — revision'


def leaf(error):
    """The first real exception inside the SDK's task-group wrappers, so failures read plainly."""
    while isinstance(error, BaseExceptionGroup):
        error = error.exceptions[0]
    return error


def payload(result):
    """The JSON object a non-recall tool returned."""
    assert not result.isError, result.content
    if result.structuredContent is not None:
        return result.structuredContent
    return json.loads(result.content[0].text)


def error_text(result):
    assert result.isError, f'expected a tool error, got {result.content}'
    return ' '.join(part.text for part in result.content)


class MCPTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.server = start_server(self)

    @asynccontextmanager
    async def session(self, credential='agent-session'):
        args = [str(SERVICE / 'mcp_server.py'), '--config', str(self.server.config(credential))]
        try:
            async with stdio_client(StdioServerParameters(command=sys.executable, args=args)) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    yield session
        except BaseExceptionGroup as group:
            raise leaf(group) from None

    async def test_session_catalog_and_text_only_recall(self):
        self.server.seed('/global/keep.md', 'Keep it simple', 'Remove what is not needed.', pinned=True)
        async with self.session() as session:
            tools = {t.name: t for t in (await session.list_tools()).tools}
            self.assertEqual(set(tools), SESSION_TOOLS)
            self.assertFalse(set(tools) & REMOVED_TOOLS)
            for name in SESSION_TOOLS - {'memory_observe', 'memory_useful'}:
                self.assertTrue(tools[name].annotations.readOnlyHint, name)
            for name in ('memory_observe', 'memory_useful'):
                self.assertFalse(tools[name].annotations.readOnlyHint, name)
                self.assertFalse(tools[name].annotations.destructiveHint, name)
            self.assertEqual(set(tools['memory_useful'].inputSchema['properties']), {'path', 'why'})
            # X25: role and topics are gone; X15: so are project_id, limit and budget.
            self.assertEqual(set(tools['memory_recall'].inputSchema['properties']), {'query', 'project'})
            status = payload(await session.call_tool('memory_status', {}))
            for key in ('store_id', 'revision', 'memories', 'pinned_chars', 'observations_pending',
                        'dreams_running', 'mirror_dir', 'mirror_stale'):
                self.assertIn(key, status)
            self.assertEqual((status['revision'], status['memories']), (1, 1))
            recall = await session.call_tool('memory_recall', {})
            self.assertFalse(recall.isError)
            self.assertIsNone(recall.structuredContent)
            self.assertEqual(len(recall.content), 1)
            self.assertIsInstance(recall.content[0], TextContent)
            self.assertTrue(recall.content[0].text.startswith(START_HEADING))
            self.assertIn('Remove what is not needed.', recall.content[0].text)
            nothing = await session.call_tool('memory_recall', {'query': 'kubernetes ingress yaml'})
            self.assertIsNone(nothing.structuredContent)
            self.assertEqual(nothing.content[0].text, 'No memory matches "kubernetes ingress yaml".\n')
            listed = payload(await session.call_tool('memory_list', {}))
            self.assertEqual([m['path'] for m in listed['data']], ['/global/keep.md'])
            self.assertNotIn('content', listed['data'][0])
            read = payload(await session.call_tool('memory_read', {'path': '/global/keep.md'}))
            self.assertIn('Remove what is not needed.', read['content'])
            self.assertEqual(read['content_sha256'], sha(read['content']))
            self.assertTrue((await session.call_tool('memory_read', {'path': '/global/missing.md'})).isError)

    async def test_adapter_has_no_maintenance_mode(self):
        result = subprocess.run([sys.executable, str(SERVICE / 'mcp_server.py'), '--config',
            str(self.server.config('agent-session')), '--maintenance-config', str(self.server.config('admin'))],
            capture_output=True, text=True, timeout=20, stdin=subprocess.DEVNULL)
        self.assertEqual(result.returncode, 2)
        self.assertIn('--maintenance-config', result.stderr)

    async def test_observe_only_and_no_version_reads(self):
        self.server.seed('/global/existing.md', 'Existing')
        candidate = content('Candidate', 'Observed in a session.')
        async with self.session() as session:
            observed = await session.call_tool('memory_observe', {'path': '/global/candidate.md', 'content': candidate})
            self.assertFalse(observed.isError, observed.content)
            self.assertIn('id', payload(observed))
            self.assertIn('400', error_text(await session.call_tool('memory_observe',
                {'path': '/global/bad.md', 'content': 'no frontmatter'})))
            # Live memories only: a version id is not a path, and version history needs `review`.
            version = self.server.api('GET', '/v1/memory_stores/live/memory_versions')[1]['data'][0]['id']
            self.assertTrue((await session.call_tool('memory_read', {'path': version})).isError)
        dream = self.server.api('POST', '/v1/dreams', {'sessions': []})[1]
        [observation] = dream['observations']
        body = observation['body'] if isinstance(observation['body'], dict) else json.loads(observation['body'])
        self.assertEqual((body['path'], body['content'], body['created_by']),
                         ('/global/candidate.md', candidate, 'token:agent-session'))
        self.assertEqual(self.server.api('GET', '/v1/status')[1]['memories'], 1)  # the live store is unchanged

    async def test_useful_marks_a_live_memory_only(self):
        self.server.seed('/global/existing.md', 'Existing')
        async with self.session() as session:
            marked = await session.call_tool('memory_useful', {'path': '/global/existing.md', 'why': 'Saved a retry'})
            self.assertFalse(marked.isError, marked.content)
            self.assertIsNone(marked.structuredContent)
            self.assertEqual(payload(marked)['path'], '/global/existing.md')
            self.assertIn('404', error_text(await session.call_tool('memory_useful',
                {'path': '/global/missing.md', 'why': 'w'})))
        [mark] = self.server.recall_entries()
        self.assertEqual((mark['view'], mark['client'], mark['path'], mark['why']),
                         ('useful', 'agent-session', '/global/existing.md', 'Saved a retry'))

    async def test_a_credential_without_observe_is_refused_by_the_service(self):
        self.server.seed('/global/existing.md', 'Existing')
        async with self.session(credential='reader') as session:
            message = error_text(await session.call_tool('memory_observe',
                {'path': '/global/o.md', 'content': content('O')}))
            self.assertIn('403', message)
            self.assertNotIn('fixture-reader', message)
            self.assertIn('403', error_text(await session.call_tool('memory_useful',
                {'path': '/global/existing.md', 'why': 'w'})))
        self.assertEqual(self.server.recall_entries(), [])
        self.assertEqual(self.server.api('GET', '/v1/status')[1]['observations_pending'], 0)

    async def test_recall_with_project_finds_project_memories(self):
        root = '/home/sai/Projects/mesh'
        self.server.seed(project_path(root), 'Mesh rule', 'Run the mesh tests first.')
        async with self.session() as session:
            tool = next(t for t in (await session.list_tools()).tools if t.name == 'memory_recall')
            self.assertIn('canonical', tool.description.lower())  # X20: tell the agent to pass the project root
            without = await session.call_tool('memory_recall', {})
            self.assertNotIn('Run the mesh tests first.', without.content[0].text)
            with_project = await session.call_tool('memory_recall', {'project': root})
            self.assertIn('Run the mesh tests first.', with_project.content[0].text)
            self.assertIn('## This project', with_project.content[0].text)

    async def test_offline_service_is_tool_error_not_empty_memory(self):
        self.server.seed('/global/keep.md', 'Keep', pinned=True)
        async with self.session() as session:
            self.server.stop()
            for name, args in [('memory_recall', {}), ('memory_recall', {'query': 'keep'}),
                               ('memory_status', {}), ('memory_list', {}), ('memory_read', {'path': '/global/keep.md'}),
                               ('memory_observe', {'path': '/global/x.md', 'content': content('X')}),
                               ('memory_useful', {'path': '/global/keep.md', 'why': 'w'})]:
                result = await session.call_tool(name, args)
                self.assertIn('unreachable', error_text(result), name)
                self.assertNotIn('No memory matches', error_text(result))


if __name__ == '__main__':
    unittest.main()
