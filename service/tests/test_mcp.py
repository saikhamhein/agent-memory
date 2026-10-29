"""Official SDK client -> stdio adapter -> real Rust HTTP server, using a scratch store."""
from contextlib import asynccontextmanager
from pathlib import Path
import sys
import unittest

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from service.tests import test_client

SERVICE = test_client.SERVICE


class MCPTests(unittest.IsolatedAsyncioTestCase):
    setUp = test_client.ClientTests.setUp
    stop_server = test_client.ClientTests.stop_server

    @asynccontextmanager
    async def session(self, maintenance=True, maintenance_role='maintainer'):
        args = [str(SERVICE / 'mcp_server.py'), '--config', str(self.root / 'reader.json')]
        if maintenance:
            args += ['--maintenance-config', str(self.root / (maintenance_role + '.json'))]
        async with stdio_client(StdioServerParameters(command=sys.executable, args=args)) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                yield session

    async def test_readonly_catalog_and_error_reporting(self):
        async with self.session(maintenance=False) as session:
            tools = (await session.list_tools()).tools
            self.assertEqual({t.name for t in tools}, {'memory_status', 'memory_recall'})
            self.assertTrue(all(t.annotations.readOnlyHint for t in tools))
            status = await session.call_tool('memory_status', {})
            self.assertFalse(status.isError)
            self.assertEqual(status.structuredContent['revision'], 0)
            recall = await session.call_tool('memory_recall', {'query': 'test'})
            self.assertFalse(recall.isError)
            self.assertEqual(recall.structuredContent['memories'], [])
            invalid = await session.call_tool('memory_recall', {'budget': 1})
            self.assertTrue(invalid.isError)
            unknown = await session.call_tool('memory_dream', {})
            self.assertTrue(unknown.isError)

    async def test_full_workflow_preserves_review_and_concurrency_guards(self):
        async with self.session() as session:
            tools = {t.name: t for t in (await session.list_tools()).tools}
            self.assertEqual(len(tools), 10)
            self.assertFalse(tools['memory_promote'].annotations.readOnlyHint)
            self.assertTrue(tools['memory_promote'].annotations.destructiveHint)
            self.assertTrue(tools['memory_diff'].annotations.readOnlyHint)
            memory = {'id': 'fixture', 'title': 'Fixture', 'body': 'Check existing context.',
                'scope': {'kind': 'global'}, 'confidence': 'explicit', 'tags': [],
                'evidence': [{'source': 'test', 'locator': 'mcp:1', 'note': 'Fixture only'}]}
            observed = await session.call_tool('memory_observe', {'memory': memory})
            self.assertFalse(observed.isError)
            dream = await session.call_tool('memory_dream', {})
            self.assertFalse(dream.isError)
            proposal = dream.structuredContent['proposal']
            proposal.update(memories=[memory], summary='Fixture', coverage='MCP test', reviewed=True)
            diff = await session.call_tool('memory_diff', {'proposal': proposal})
            self.assertEqual(diff.structuredContent['added'], [memory])
            self.assertFalse((await session.call_tool('memory_validate', {'proposal': proposal})).isError)
            self.assertFalse((await session.call_tool('memory_promote', {'proposal': proposal})).isError)
            stale = await session.call_tool('memory_promote', {'proposal': proposal})
            self.assertTrue(stale.isError)
            self.assertIn('409', stale.content[0].text)
            recalled = await session.call_tool('memory_recall', {})
            self.assertEqual(recalled.structuredContent['memories'], [memory])
            saved = await session.call_tool('memory_get_dream', {'dream_id': proposal['dream_id']})
            self.assertEqual(saved.structuredContent['proposal'], proposal)
            exported = await session.call_tool('memory_export', {})
            self.assertEqual(exported.structuredContent['revision'], 1)
            rolled_back = await session.call_tool('memory_rollback', {'revision': 0, 'expected': 1, 'reason': 'Fixture'})
            self.assertEqual(rolled_back.structuredContent['revision'], 2)

    async def test_backend_permissions_are_not_bypassed(self):
        async with self.session(maintenance_role='reader') as session:
            denied = await session.call_tool('memory_dream', {})
            self.assertTrue(denied.isError)
            self.assertIn('403', denied.content[0].text)
            self.assertNotIn('fixture-reader', denied.content[0].text)

    async def test_offline_service_is_tool_error_not_empty_memory(self):
        async with self.session() as session:
            self.stop_server()
            result = await session.call_tool('memory_recall', {})
            self.assertTrue(result.isError)
            self.assertIn('unreachable', result.content[0].text)


if __name__ == '__main__':
    unittest.main()
