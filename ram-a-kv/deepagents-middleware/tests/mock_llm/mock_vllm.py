"""Local OpenAI-compatible mock; hashes originate in the HTTP response."""

import json
import threading
import time
from uuid import uuid4
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


@contextmanager
def mock_vllm(log_path):
    exchanges = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if self.path != '/v1/chat/completions' or request.get('stream'):
                self.send_error(400, 'Only non-streaming /v1/chat/completions is supported')
                return
            if not request.get('messages'):
                self.send_error(400, 'messages are required')
                return
            turn = len(exchanges) + 1
            # These are fixture cache identities, not hashes of the prompt.
            # Shift one handle in the second response to exercise map replacement.
            block_ids = [f'{value:032x}' for value in range(turn, turn + 28)]
            response = {
                'id': f"chatcmpl-{uuid4()}",
                'object': 'chat.completion',
                'created': int(time.time()),
                'model': request['model'],
                'choices': [{'index': 0, 'message': {
                    'role': 'assistant', 'content': f'mock reply {turn}'
                }, 'finish_reason': 'stop'}],
                'usage': {'prompt_tokens': 5, 'completion_tokens': 3, 'total_tokens': 8},
                'kv_transfer_params': {'ucm_block_ids': block_ids, 'prefech_rank': 3},
            }
            exchanges.append({'request': request, 'response': response})
            with log_path.open('a', encoding='utf-8') as log:
                log.write(json.dumps(exchanges[-1], ensure_ascii=False) + '\n')
            encoded = json.dumps(response).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f'http://127.0.0.1:{server.server_port}/v1', exchanges
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
