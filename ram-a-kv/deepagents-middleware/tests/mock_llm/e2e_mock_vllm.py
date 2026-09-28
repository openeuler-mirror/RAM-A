#!/usr/bin/env python3
"""Agent -> HTTP mock vLLM -> real RAM-A, verified against daemon logs."""

import argparse
import asyncio
import json
import os
import re
import socket
import subprocess
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from uuid import UUID

import httpx
from deepagents import create_deep_agent

from mock_vllm import mock_vllm
from ram_a_middleware.sdk import AsyncRamAClient, RamAClient
from ram_a_middleware.deepagents import RamAKvMiddleware
from langchain_openai import ChatOpenAI
from langchain.agents.middleware.types import AgentMiddleware

def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


class AgentRequestIds(AgentMiddleware):
    """Test-only stand-in for an application's existing request-ID producer."""
    def __init__(self):
        self.counter = 0

    def prepare(self, request):
        self.counter += 1
        settings = dict(request.model_settings)
        settings['extra_body'] = {'request_id': f'test-agent-request-{self.counter}'}
        return request.override(model_settings=settings)

    def wrap_model_call(self, request, handler):
        return handler(self.prepare(request))

    async def awrap_model_call(self, request, handler):
        return await handler(self.prepare(request))


class RecordingTransport(httpx.HTTPTransport):
    def __init__(self, events):
        super().__init__()
        self.events = events

    def handle_request(self, request):
        body = json.loads(request.content)
        response = super().handle_request(request)
        response.read()
        self.events.append({'request': body, 'ack': response.json()})
        return response


class RecordingAsyncTransport(httpx.AsyncHTTPTransport):
    def __init__(self, events):
        super().__init__()
        self.events = events

    async def handle_async_request(self, request):
        response = await super().handle_async_request(request)
        await response.aread()
        self.events.append({'request': json.loads(request.content), 'ack': response.json()})
        return response


async def invoke_async(agent, inputs, config):
    return await asyncio.wait_for(agent.ainvoke(inputs, config=config), timeout=30)


@contextmanager
def ram_a(binary, output):
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    url = f'http://127.0.0.1:{port}'
    config = output / 'ram-a.toml'
    config.write_text('\n'.join((
        f'listen_addr = "127.0.0.1:{port}"',
        'noop_backend = true',
        'turn_start_prefetch = true',
        'trace_events = true',
        'debug_enabled = false',
        f'session_store_path = {json.dumps(str(output / "sessions.db"))}',
    )) + '\n')
    env = dict(os.environ, RAM_A_KV_CONFIG=str(config),
               RUST_LOG='info,ram_a_kv::api=debug', NO_COLOR='1')
    with (output / 'ram-a.log').open('wb') as log:
        process = subprocess.Popen([str(binary)], cwd=output, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            with httpx.Client(timeout=0.5, trust_env=False) as client:
                deadline = time.monotonic() + 10
                while True:
                    require(process.poll() is None, 'RAM-A exited before readiness; see ram-a.log')
                    try:
                        if client.post(url + '/event', json={'type': 'health'}).json().get('ok'):
                            break
                    except (httpx.HTTPError, ValueError):
                        pass
                    require(time.monotonic() < deadline, 'RAM-A readiness timeout; see ram-a.log')
                    time.sleep(0.1)
            yield url
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def check_evidence(output, events, exchanges, stored):
    log = re.sub(r'\x1b\[[0-9;]*m', '', (output / 'ram-a.log').read_text())
    records = [json.loads(line.split('evidence=', 1)[1]) for line in log.splitlines()
               if 'ram-a-kv lifecycle evidence' in line and 'evidence=' in line]
    records = [record for record in records if record['session_id'] == 'deepagents:mock-vllm-e2e']
    types = ['agent_event', 'turn_start', 'turn_end'] * 2
    require([r['type'] for r in records] == types, ('daemon evidence missing or out of order', records))
    require([e['request']['type'] for e in events] == types, events)
    require(len(exchanges) == 2, exchanges)
    require(len({r['event_id'] for r in records}) == 6, records)
    for event, record in zip(events, records):
        UUID(record['event_id'])
        require(event['ack']['ok'], event)
        require(event['ack']['data'] == record['result'], (event, record))
        for key in ('event_id', 'session_id', 'agent_turn_id', 'model_request_id'):
            require(event['request'].get(key) == record[key], (event, record))
    for index in range(2):
        agent, start, end = records[index * 3:index * 3 + 3]
        request, response = exchanges[index]['request'], exchanges[index]['response']
        require({r['session_id'] for r in (agent, start, end)} == {'deepagents:mock-vllm-e2e'}, records)
        require(len({r['agent_turn_id'] for r in (agent, start, end)}) == 1, records)
        require((agent['event_name'], agent['event_action'], agent['message_count']) == ('agent', 'start', 1), agent)
        require(agent['result'] == {'accepted': True, 'duplicate': False}, agent)
        require(start['model_request_id'] == end['model_request_id'] == request['request_id'], records)
        require(request['messages'][-1] == {'role': 'user', 'content': f'message {index + 1}'}, request)
        transfer = response['kv_transfer_params']
        require(set(transfer) == {'ucm_block_ids', 'prefech_rank'}, transfer)
        require(type(transfer['prefech_rank']) is int and transfer['prefech_rank'] == 3, transfer)
        hashes = transfer['ucm_block_ids']
        require(isinstance(hashes, list) and len(hashes) == 28, transfer)
        require(all(isinstance(value, str) and re.fullmatch(r'[0-9a-fA-F]{32}', value)
                    for value in hashes), transfer)
        require(len(set(hashes)) == 28, transfer)
        sent_end = events[index * 3 + 2]['request']
        require(sent_end['kv_transfer_params']['chunk_hashes'] == hashes, sent_end)
        metadata = sent_end['debug_context']['response_metadata']['messages'][0]
        require(metadata['kv_transfer_params'] == transfer, metadata)
        require(end['chunk_hashes'] == hashes, (end, response))
        require(end['result']['map_updated'] and end['result']['chunk_count'] == len(hashes), end)
        require(not start['result']['backend_degraded'] and not end['result']['backend_degraded'], records)
        require(any('[trace] session map' in line and f'turn_count={index + 1}' in line
                    and f'chunk_hashes={json.dumps(hashes)}' in line for line in log.splitlines()), log)
    require(records[0]['agent_turn_id'] != records[3]['agent_turn_id'], records)
    require(records[1]['model_request_id'] != records[4]['model_request_id'], records)
    require(records[1]['result']['prefetch_sent'] is False and records[1]['result']['prefetch_count'] == 0, records[1])
    require(records[4]['result']['prefetch_sent'] is True and records[4]['result']['prefetch_count'] == 28, records[4])
    first_ids = exchanges[0]['response']['kv_transfer_params']['ucm_block_ids']
    second_ids = exchanges[1]['response']['kv_transfer_params']['ucm_block_ids']
    require(len(set(first_ids) & set(second_ids)) == 27, (first_ids, second_ids))
    require(records[5]['result']['evicted_count'] == 1, records[5])
    require(stored['chunk_hashes'] == second_ids, stored)
    (output / 'evidence.json').write_text(json.dumps(records, indent=2) + '\n')
    print('[PASS] Agent sent 2 HTTP requests to mock vLLM and received both replies')
    print('[PASS] Each response preserves 28 unique 32-hex ucm_block_ids and prefech_rank=3')
    print('[PASS] RAM-A log: agent/start accepted twice; turn and request IDs match')
    print('[PASS] RAM-A log: turn_end received response hashes and updated the session map')
    print('[PASS] RAM-A log: first prefetch=0; second prefetch_sent=true, prefetch_count=28')
    print('[PASS] RAM-A log: second turn_end replaced hashes; session_map readback matches')


def check_regressions():
    import io
    import logging
    from types import SimpleNamespace
    from langchain.agents.middleware.types import ModelRequest, ModelResponse
    from langchain_core.messages import AIMessage
    from langchain_core.language_models.fake_chat_models import GenericFakeChatModel
    from langchain_core.tools import tool
    from ram_a_middleware.deepagents import json_safe
    from ram_a_middleware.sdk import AgentEvent, CallContext, CallResult

    events = []
    class StrictTransport(httpx.BaseTransport):
        closed = False
        def handle_request(self, request):
            require(not self.closed, 'transport reused after close')
            events.append(json.loads(request.content))
            return httpx.Response(200, json={'ok': True, 'data': {}})
        def close(self):
            self.closed = True
    transport = StrictTransport()
    client = RamAClient('http://ram-a.test', transport=transport)
    middleware = RamAKvMiddleware(client, response_max_bytes=128)
    model = ChatOpenAI(api_key='local-test', extra_body={'request_id': 'existing-model-id'})
    runtime = SimpleNamespace(context={'session_id': 0, 'password': 'dummy-private-value'})
    state = middleware.before_agent({'messages': []}, runtime)
    require(state['_ram_a_session_id'] == 'deepagents:0', state)
    request = ModelRequest(model=model, messages=[], state=state, runtime=runtime,
                           model_settings={'extra_body': {'request_id': 'existing-call-id'},
                                           'extra_headers': {'Authorization': 'dummy-header'}})
    response = ModelResponse(result=[AIMessage(content='reply', response_metadata={
        'kv_transfer_params': {'ucm_block_ids': ['a', 'b']}, 'oversized': 'x' * 100000})])
    seen = []
    def handler(received):
        seen.append(received)
        require(received.model_settings == request.model_settings, 'request settings changed')
        return response
    require(middleware.wrap_model_call(request, handler) is response, 'response replaced')
    require(events[-2]['model_request_id'] == events[-1]['model_request_id'] == 'existing-call-id', events)
    require(type(model) is ChatOpenAI and model.extra_body['request_id'] == 'existing-model-id', model)
    wire = json.dumps(events)
    require('dummy-private-value' not in wire and 'dummy-header' not in wire and 'oversized' not in wire, wire)
    require(len(json.dumps(events[-1]['debug_context'], separators=(',', ':')).encode()) <= 128, events[-1])
    no_id = request.override(model=ChatOpenAI(api_key='local-test'), model_settings={})
    middleware.wrap_model_call(no_id, lambda r: response)
    require('model_request_id' not in events[-1] and 'model_request_id' not in events[-2], events)
    events.clear()
    state = middleware.before_agent({'messages': []}, SimpleNamespace(context={}))
    middleware.wrap_model_call(no_id.override(state=state), lambda r: response)
    require(not events, 'anonymous invocation must not persist cache mappings')
    for values in (None, [], 'wrong', [''], ['x' * 101], ['x'] * 1001, ['\ud800']):
        invalid = ModelResponse(result=[AIMessage(content='reply', response_metadata={
            'kv_transfer_params': {'ucm_block_ids': values}})])
        events.clear()
        require(middleware.wrap_model_call(request, lambda r: invalid) is invalid, 'invalid KV interrupted model')
        require([e['type'] for e in events] == ['turn_start'], events)
    def broken(*_args):
        raise RuntimeError('failure')
    middleware.cache_identity_provider = broken
    require(middleware.wrap_model_call(request, lambda r: response) is response, 'provider failure interrupted model')
    try:
        middleware.wrap_model_call(request, broken)
    except RuntimeError:
        pass
    else:
        raise AssertionError('model exceptions must propagate')
    client.close()
    require(not transport.closed, 'borrowed transport was closed')
    require(not client.begin_model_call(CallContext('s', 't')).ok, 'closed client reopened')
    transport.close()

    logs = io.StringIO()
    log_handler = logging.StreamHandler(logs)
    logger = logging.getLogger('ram_a_middleware')
    logger.addHandler(log_handler)
    def unavailable(req):
        raise httpx.ConnectError('simulated failure', request=req)
    with RamAClient('http://ram-a.test', transport=httpx.MockTransport(unavailable)) as failed:
        failure_middleware = RamAKvMiddleware(failed)
        require(failure_middleware.wrap_model_call(request, lambda r: response) is response, 'HTTP failure interrupted model')
    logger.removeHandler(log_handler)
    require('existing-call-id' in logs.getvalue() and 'turn_start' in logs.getvalue(), logs.getvalue())
    with RamAClient('http://ram-a.test', transport=httpx.MockTransport(
            lambda r: httpx.Response(200, json={'ok': False, 'error': 'business failure'}))) as bad:
        require(bad.begin_model_call(CallContext('s', 't')).error_code == 'daemon_error', 'misleading HTTP 200 error')
    attempts = []
    def retry(req):
        attempts.append(json.loads(req.content))
        return httpx.Response(200, json=[] if len(attempts) == 1 else {'ok': True})
    with RamAClient('http://ram-a.test', retries=1, transport=httpx.MockTransport(retry)) as retrying:
        require(retrying.agent_event(AgentEvent('agent', 'start', 's')).ok, attempts)
    require(len(attempts) == 2 and attempts[0]['event_id'] == attempts[1]['event_id'], attempts)

    class FakeModel(GenericFakeChatModel):
        async def _agenerate(self, messages, stop=None, run_manager=None, **kwargs):
            return self._generate(messages, stop=stop, **kwargs)

        def bind_tools(self, *args, **kwargs):
            return self
    @tool
    def echo(text: str) -> str:
        """Return text unchanged."""
        return text
    def reply(content, **kwargs):
        return AIMessage(content=content, response_metadata={'kv_transfer_params': {'ucm_block_ids': ['a']}}, **kwargs)
    events.clear()
    sync_transport = httpx.MockTransport(lambda r: (events.append(json.loads(r.content)) or httpx.Response(200, json={'ok': True})))
    with RamAClient('http://ram-a.test', transport=sync_transport) as graph_client:
        fake = FakeModel(messages=iter([reply('', tool_calls=[{'name': 'echo', 'args': {'text': 'ok'}, 'id': 'call-1'}]), reply('done')]))
        graph = create_deep_agent(model=fake, tools=[echo], middleware=[RamAKvMiddleware(graph_client)])
        output = graph.invoke({'messages': [{'role': 'user', 'content': 'hello'}]}, config={'configurable': {'thread_id': 'tool-loop'}})
        require(output['messages'][-1].content == 'done', output)
    require([e['type'] for e in events] == ['agent_event', 'turn_start', 'turn_end', 'turn_start', 'turn_end'], events)
    require(len({e['agent_turn_id'] for e in events}) == 1, events)

    async def concurrent():
        class StrictAsyncTransport(httpx.AsyncBaseTransport):
            closed = False
            async def handle_async_request(self, req):
                require(not self.closed, 'async transport closed')
                events.append(json.loads(req.content))
                await asyncio.sleep(0)
                return httpx.Response(200, json={'ok': True})
            async def aclose(self):
                self.closed = True
        borrowed = StrictAsyncTransport()
        async with AsyncRamAClient('http://ram-a.test', transport=borrowed) as async_client:
            with RamAClient('http://ram-a.test') as sync_client:
                mw = RamAKvMiddleware(sync_client, async_client=async_client)
                fake = FakeModel(messages=iter([reply('one'), reply('two')]))
                graph = create_deep_agent(model=fake, middleware=[mw])
                await asyncio.wait_for(asyncio.gather(*[graph.ainvoke({'messages': [{'role': 'user', 'content': name}]},
                    config={'configurable': {'thread_id': name}}) for name in ('concurrent-a', 'concurrent-b')]), timeout=15)
        require(not borrowed.closed, 'borrowed async transport closed')
        await borrowed.aclose()
    events.clear()
    asyncio.run(concurrent())
    for name in ('concurrent-a', 'concurrent-b'):
        group = [e for e in events if e['session_id'] == f'deepagents:{name}']
        require([e['type'] for e in group] == ['agent_event', 'turn_start', 'turn_end'], group)
        require(len({e['agent_turn_id'] for e in group}) == 1, group)
    require(len({e['agent_turn_id'] for e in events}) == 2, events)
    cycle = {}; cycle['self'] = cycle
    for limit in (2, 18, 128):
        snapshot = json_safe({'cycle': cycle, 'password': 'dummy-secret', 'large': 'x' * 10000}, max_bytes=limit)
        encoded = json.dumps(snapshot, separators=(',', ':'))
        require(len(encoded.encode()) <= limit and 'dummy-secret' not in encoded, encoded)
    print('[PASS] Regressions: upstream IDs, no anonymous state, privacy, limits, client lifetime, failures, tool loop and concurrency')


def check_daemon_contract(url):
    payload = {'type': 'agent_event', 'event_id': 'contract-check', 'session_id': 'contract-only',
               'event_name': 'agent', 'event_action': 'start', 'payload': {}}
    with httpx.Client(trust_env=False, timeout=2) as probe:
        first = probe.post(url + '/event', json=payload).json()
        second = probe.post(url + '/event', json=payload).json()
        require(first['ok'] and first['data']['duplicate'] is False, first)
        require(second['ok'] and second['data']['duplicate'] is True, second)
        for change in ({'event_id': 'x' * 129}, {'event_id': ' '}, {'payload': []}, {'session_id': ''}):
            rejected = probe.post(url + '/event', json={**payload, **change}).json()
            require(rejected['ok'] is False, rejected)
    print('[PASS] RAM-A handler: duplicate ACK, ID length and required-field validation')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--daemon-bin', type=Path,
                        default=os.environ.get('RAM_A_KV_DAEMON_BIN') or None,
                        help='RAM-A daemon binary (or set RAM_A_KV_DAEMON_BIN)')
    parser.add_argument('--async', dest='async_mode', action='store_true', help='Run both turns with ainvoke')
    parser.add_argument('--output-dir', type=Path, help='New directory for retained logs and evidence')
    args = parser.parse_args()
    if args.daemon_bin is None:
        parser.error('provide --daemon-bin or RAM_A_KV_DAEMON_BIN; '
                     'build RAM-A first (cargo build in the ram-a-kv directory)')
    binary = args.daemon_bin.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f'Daemon is missing or not executable: {binary}')
    output = args.output_dir.expanduser().resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix='ram-a-e2e-'))
    if args.output_dir:
        output.mkdir(parents=True, exist_ok=False)
    print(f'Artifacts: {output}', flush=True)
    events = []
    # Both providers use loopback only; external credentials/tracing are unnecessary.
    os.environ['LANGSMITH_TRACING'] = 'false'
    os.environ['LANGCHAIN_TRACING_V2'] = 'false'
    try:
        check_regressions()
        with ram_a(binary, output) as daemon_url, mock_vllm(output / 'mock-vllm.jsonl') as (model_url, exchanges):
            check_daemon_contract(daemon_url)
            with httpx.Client(trust_env=False) as model_http:
                model = ChatOpenAI(
                    model='mock-vllm', base_url=model_url, api_key='local-test',
                    max_retries=0, timeout=5, use_responses_api=False, streaming=False,
                    http_client=model_http,
                )
                client = RamAClient(daemon_url, timeout=2, transport=RecordingTransport(events))
                if args.async_mode:
                    async_client = AsyncRamAClient(daemon_url, timeout=2, transport=RecordingAsyncTransport(events))
                    middleware = RamAKvMiddleware(client, async_client=async_client)
                else:
                    middleware = RamAKvMiddleware(client)
                agent = create_deep_agent(model=model, middleware=[AgentRequestIds(), middleware])
                with asyncio.Runner() as runner:
                    for turn in (1, 2):
                        inputs = {'messages': [{'role': 'user', 'content': f'message {turn}'}]}
                        config = {'configurable': {'thread_id': 'mock-vllm-e2e'}}
                        result = (runner.run(invoke_async(agent, inputs, config)) if args.async_mode
                                  else agent.invoke(inputs, config=config))
                        require(result['messages'][-1].content == f'mock reply {turn}', result)
                        require('_ram_a_turn_id' not in result and '_ram_a_session_id' not in result, result)
                    runner.run(middleware.aclose())
                    if args.async_mode:
                        runner.run(async_client.aclose())
                        runner.run(async_client.transport.aclose())
                    runner.run(model.root_async_client.close())
                client.close()
                client.transport.close()
                with httpx.Client(trust_env=False, timeout=2) as probe:
                    reply = probe.post(daemon_url + '/event', json={
                        'type': 'session_map', 'session_id': 'deepagents:mock-vllm-e2e'}).json()
                    require(reply.get('ok'), reply)
                    stored = reply['data']
        # Process has exited and flushed its logs before checking evidence.
        check_evidence(output, events, exchanges, stored)
        print('RESULT: PASS')
        return 0
    except Exception as exc:
        print(f'RESULT: FAIL — {exc}')
        print(f'See {output / "ram-a.log"}')
        return 1
    finally:
        (output / 'events.json').write_text(json.dumps(events, indent=2) + '\n')


if __name__ == '__main__':
    raise SystemExit(main())
