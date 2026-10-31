"""Official SDK client -> stdio adapter -> real Rust HTTP server (design §4.3 C), on a scratch store.

Tool results other than memory_recall are the API's JSON object (structured content, with the same
JSON as text). memory_recall is text only.
"""
from contextlib import asynccontextmanager
import json
import sys
import unittest

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.types import TextContent
from service.tests.harness import SERVICE, content, sha, start_server

READ_TOOLS = {'memory_status', 'memory_recall', 'memory_list', 'memory_read', 'memory_versions'}
MAINTENANCE_TOOLS = READ_TOOLS | {'memory_observe', 'memory_write', 'memory_delete', 'memory_redact',
    'memory_dream_start', 'memory_dream_get', 'memory_dream_adopt', 'memory_dream_cancel'}
V1_TOOLS = {'memory_dream', 'memory_get_dream', 'memory_validate', 'memory_diff', 'memory_promote',
    'memory_rollback', 'memory_export'}
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
    async def session(self, maintenance='admin', reader='reader'):
        args = [str(SERVICE / 'mcp_server.py'), '--config', str(self.server.config(reader))]
        if maintenance:
            args += ['--maintenance-config', str(self.server.config(maintenance))]
        try:
            async with stdio_client(StdioServerParameters(command=sys.executable, args=args)) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    yield session
        except BaseExceptionGroup as group:
            raise leaf(group) from None

    async def test_read_catalog_and_text_only_recall(self):
        self.server.seed('/global/keep.md', 'Keep it simple', 'Remove what is not needed.', pinned=True)
        async with self.session(maintenance=None) as session:
            tools = (await session.list_tools()).tools
            self.assertEqual({t.name for t in tools}, READ_TOOLS)
            self.assertTrue(all(t.annotations.readOnlyHint for t in tools))
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
            versions = payload(await session.call_tool('memory_versions', {'path': '/global/keep.md'}))
            self.assertEqual([v['operation'] for v in versions['data']], ['created'])
            self.assertTrue((await session.call_tool('memory_read', {'path': '/global/missing.md'})).isError)
            # The removed limit/budget knobs are not part of the tool any more.
            props = next(t for t in tools if t.name == 'memory_recall').inputSchema['properties']
            self.assertEqual(set(props), {'query', 'project', 'role', 'topics'})

    async def test_maintenance_catalog_has_no_v1_tools(self):
        async with self.session() as session:
            tools = {t.name: t for t in (await session.list_tools()).tools}
            self.assertEqual(len(tools), 13)
            self.assertEqual(set(tools), MAINTENANCE_TOOLS)
            self.assertFalse(set(tools) & V1_TOOLS)
            for name in READ_TOOLS:
                self.assertTrue(tools[name].annotations.readOnlyHint, name)
            for name in ('memory_delete', 'memory_redact', 'memory_dream_adopt'):
                self.assertTrue(tools[name].annotations.destructiveHint, name)
            for name in MAINTENANCE_TOOLS - READ_TOOLS:
                self.assertFalse(tools[name].annotations.readOnlyHint, name)
            unknown = await session.call_tool('memory_promote', {})
            self.assertTrue(unknown.isError)

    async def test_write_by_path_with_preconditions(self):
        path, other = '/global/note.md', '/topics/hardware/note.md'
        first = content('Note', 'First text.')
        async with self.session() as session:
            created = payload(await session.call_tool('memory_write', {'path': path, 'content': first}))
            self.assertEqual((created['path'], created['content_sha256']), (path, sha(first)))
            clash = error_text(await session.call_tool('memory_write', {'path': path, 'content': first}))
            self.assertIn('path_exists', clash)  # no expected_sha256 means create
            second = content('Note', 'Second text.')
            updated = payload(await session.call_tool('memory_write',
                {'path': path, 'content': second, 'expected_sha256': sha(first)}))
            self.assertEqual(updated['content_sha256'], sha(second))
            self.assertNotEqual(updated['memory_version_id'], created['memory_version_id'])
            stale = error_text(await session.call_tool('memory_write',
                {'path': path, 'content': content('Note', 'Lost update.'), 'expected_sha256': sha(first)}))
            self.assertIn('precondition_failed', stale)
            self.assertIn(path, stale)
            self.assertIn('re-read', stale.lower())
            self.assertEqual(payload(await session.call_tool('memory_read', {'path': path}))['content'], second)
            renamed = payload(await session.call_tool('memory_write',
                {'path': path, 'expected_sha256': sha(second), 'new_path': other}))
            self.assertEqual((renamed['id'], renamed['path']), (created['id'], other))
            self.assertTrue((await session.call_tool('memory_read', {'path': path})).isError)
            history = payload(await session.call_tool('memory_versions', {'path': other}))
            self.assertEqual([v['operation'] for v in history['data']], ['modified', 'modified', 'created'])
            self.assertEqual({v['path'] for v in history['data']}, {path, other})
            self.assertTrue(self.server.mirror.joinpath(other.lstrip('/')).is_file())
            bad = error_text(await session.call_tool('memory_write',
                {'path': '/global/bad.md', 'content': 'no frontmatter'}))
            self.assertIn('400', bad)
            wrong = error_text(await session.call_tool('memory_delete',
                {'path': other, 'expected_sha256': sha('something else')}))
            self.assertIn('precondition_failed', wrong)
            gone = payload(await session.call_tool('memory_delete', {'path': other, 'expected_sha256': sha(second)}))
            self.assertEqual(gone['id'], created['id'])
            self.assertTrue((await session.call_tool('memory_read', {'path': other})).isError)
            latest = payload(await session.call_tool('memory_versions', {'created_by': 'token:admin'}))['data'][0]
            self.assertEqual((latest['operation'], latest['memory_id']), ('deleted', created['id']))
            self.assertEqual(payload(await session.call_tool('memory_list', {}))['data'], [])

    async def test_redact_old_version_but_not_head(self):
        async with self.session() as session:
            first = content('Secret', 'Pasted by mistake.')
            await session.call_tool('memory_write', {'path': '/global/s.md', 'content': first})
            await session.call_tool('memory_write', {'path': '/global/s.md', 'content': content('Secret', 'Clean.'),
                'expected_sha256': sha(first)})
            head, old = payload(await session.call_tool('memory_versions', {'path': '/global/s.md'}))['data']
            self.assertIn('400', error_text(await session.call_tool('memory_redact', {'version_id': head['id']})))
            done = payload(await session.call_tool('memory_redact', {'version_id': old['id']}))
            self.assertIsNotNone(done['redacted_at'])
            self.assertEqual(done['created_by'], 'token:admin')

    async def test_dream_workflow_mcp(self):
        existing = content('Existing', 'Original body.')
        self.server.seed('/global/existing.md', 'Existing', 'Original body.')
        async with self.session() as session:
            candidate = content('Candidate', 'Observed in a session.')
            self.assertFalse((await session.call_tool('memory_observe',
                {'path': '/global/candidate.md', 'content': candidate})).isError)
            started = payload(await session.call_tool('memory_dream_start', {'sessions': []}))
            dream_id, output = started['id'], started['output_store_id']
            self.assertEqual(started['status'], 'running')
            self.assertEqual(len(started['observations']), 1)
            # The output store starts as a clone of the live store, same ids and hashes.
            live = payload(await session.call_tool('memory_list', {}))['data']
            cloned = payload(await session.call_tool('memory_list', {'store': output}))['data']
            self.assertEqual([(m['id'], m['path'], m['content_sha256']) for m in cloned],
                             [(m['id'], m['path'], m['content_sha256']) for m in live])
            added = content('Zebrafish husbandry', 'Zebrafish tanks need weekly water changes.')
            changed = content('Existing', 'Revised body.')
            await session.call_tool('memory_write', {'path': '/global/zebrafish.md', 'content': added, 'store': output})
            sha_existing = live[0]['content_sha256']
            self.assertFalse((await session.call_tool('memory_write', {'path': '/global/existing.md',
                'content': changed, 'expected_sha256': sha_existing, 'store': output})).isError)
            self.assertTrue((await session.call_tool('memory_read', {'path': '/global/zebrafish.md'})).isError)
            diff = payload(await session.call_tool('memory_dream_get', {'dream_id': dream_id, 'diff': True}))['diff']
            self.assertEqual([(m['path'], m['content']) for m in diff['added']], [('/global/zebrafish.md', added)])
            self.assertEqual([(c['before']['content'], c['after']['content']) for c in diff['changed']],
                             [(existing, changed)])
            self.assertEqual(diff['removed'], [])
            listing = payload(await session.call_tool('memory_dream_get', {}))
            self.assertIn(dream_id, [d['id'] for d in listing['data']])
            review = dict(dream_id=dream_id, summary='Add zebrafish', coverage='MCP test', conflicts=[])
            refused = await session.call_tool('memory_dream_adopt', dict(review, reviewed=False))
            self.assertIn('review', error_text(refused).lower())
            self.assertEqual(payload(await session.call_tool('memory_dream_get', {'dream_id': dream_id}))['status'], 'running')
            adopted = payload(await session.call_tool('memory_dream_adopt', dict(review, reviewed=True)))
            self.assertEqual(adopted['status'], 'completed')
            found = await session.call_tool('memory_recall', {'query': 'zebrafish husbandry'})
            self.assertIn('Zebrafish tanks need weekly water changes.', found.content[0].text)
            self.assertEqual(payload(await session.call_tool('memory_read', {'path': '/global/existing.md'}))['content'], changed)
            self.assertTrue((self.server.mirror / 'global/zebrafish.md').is_file())
            versions = payload(await session.call_tool('memory_versions', {'created_by': 'dream:' + dream_id}))
            self.assertEqual(len(versions['data']), 2)  # the live store got one added and one changed memory
            late = error_text(await session.call_tool('memory_write',
                {'path': '/global/late.md', 'content': content('Late'), 'store': output}))
            self.assertIn('store_archived', late)
            # The observation was consumed by the adopted dream; a second dream can be cancelled.
            second = payload(await session.call_tool('memory_dream_start', {}))
            self.assertEqual(second['observations'], [])
            cancelled = payload(await session.call_tool('memory_dream_cancel', {'dream_id': second['id']}))
            self.assertEqual(cancelled['status'], 'canceled')
            self.assertEqual(payload(await session.call_tool('memory_dream_cancel', {'dream_id': second['id']}))['status'], 'canceled')
            self.assertTrue((await session.call_tool('memory_dream_adopt',
                dict(review, dream_id=second['id'], reviewed=True))).isError)

    async def test_reader_cannot_write(self):
        self.server.seed('/global/existing.md', 'Existing')
        listed = self.server.api('GET', '/v1/memory_stores/live/memories', role='reader')[1]['data']
        text = content('Existing', 'Changed.')
        calls = [('memory_write', {'path': '/global/new.md', 'content': content('New')}),
                 ('memory_write', {'path': '/global/existing.md', 'content': text, 'expected_sha256': listed[0]['content_sha256']}),
                 ('memory_delete', {'path': '/global/existing.md'}),
                 ('memory_observe', {'path': '/global/o.md', 'content': content('O')}),
                 ('memory_dream_start', {}), ('memory_dream_get', {})]
        async with self.session(maintenance='reader') as session:
            for name, args in calls:
                message = error_text(await session.call_tool(name, args))
                self.assertIn('403', message, name)
                self.assertNotIn('fixture-reader', message)
        self.assertEqual(self.server.api('GET', '/v1/status', role='reader')[1]['memories'], 1)

    async def test_reviewer_works_in_dream_but_cannot_change_live(self):
        self.server.seed('/global/existing.md', 'Existing')
        async with self.session(maintenance='reviewer') as session:
            started = payload(await session.call_tool('memory_dream_start', {}))
            output = started['output_store_id']
            made = await session.call_tool('memory_write',
                {'path': '/global/draft.md', 'content': content('Draft'), 'store': output})
            self.assertFalse(made.isError, made.content)
            self.assertIn('403', error_text(await session.call_tool('memory_write',
                {'path': '/global/live.md', 'content': content('Live')})))
            self.assertIn('403', error_text(await session.call_tool('memory_dream_adopt',
                dict(dream_id=started['id'], summary='s', coverage='c', conflicts=[], reviewed=True))))
            self.assertIn('403', error_text(await session.call_tool('memory_redact', {'version_id': 'memver_' + '0' * 32})))
            self.assertFalse((await session.call_tool('memory_dream_cancel', {'dream_id': started['id']})).isError)

    async def test_observer_can_only_observe(self):
        async with self.session(maintenance='observer') as session:
            ok = await session.call_tool('memory_observe', {'path': '/global/o.md', 'content': content('O')})
            self.assertFalse(ok.isError, ok.content)
            self.assertIn('403', error_text(await session.call_tool('memory_write',
                {'path': '/global/new.md', 'content': content('New')})))
        self.assertEqual(self.server.api('GET', '/v1/status')[1]['observations_pending'], 1)

    async def test_offline_service_is_tool_error_not_empty_memory(self):
        self.server.seed('/global/keep.md', 'Keep', pinned=True)
        async with self.session() as session:
            self.server.stop()
            for name, args in [('memory_recall', {}), ('memory_recall', {'query': 'keep'}),
                               ('memory_status', {}), ('memory_list', {}),
                               ('memory_write', {'path': '/global/x.md', 'content': content('X')})]:
                result = await session.call_tool(name, args)
                self.assertIn('unreachable', error_text(result), name)
                self.assertNotIn('No memory matches', error_text(result))


if __name__ == '__main__':
    unittest.main()
