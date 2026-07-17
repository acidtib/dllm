#!/usr/bin/env python3
"""Streaming inference fixture for P4.3 physical validation.

Responds to /health, /health/runtime, and /v1/chat/completions with a
controlled streaming SSE response. Accepts an X-Delay-Ms header and
X-Chunk-Count header to vary behavior for cancellation/deadline tests.
"""
import http.server
import json
import sys
import time

CHUNK_COUNT = 10
DELAY_MS = 200
PORT = 8081


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print(f"[fixture] {args[0]}", flush=True)

    def do_GET(self):
        if self.path in ("/health", "/health/runtime"):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok\n")
            return
        self.send_response(404)
        self.end_headers()

    def do_POST(self):
        if self.path == "/v1/chat/completions":
            body_len = int(self.headers.get("Content-Length", 0))
            _body = self.rfile.read(body_len) if body_len > 0 else b""

            try:
                chunk_count = int(self.headers.get("X-Chunk-Count", CHUNK_COUNT))
                delay_ms = float(self.headers.get("X-Delay-Ms", DELAY_MS))
            except (ValueError, TypeError):
                chunk_count = CHUNK_COUNT
                delay_ms = DELAY_MS

            delay_s = delay_ms / 1000.0

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()

            def write(data):
                self.wfile.write(data)
                self.wfile.flush()

            for i in range(chunk_count):
                chunk = json.dumps(
                    {
                        "choices": [
                            {
                                "delta": {"content": f"chunk-{i}"},
                                "index": 0,
                            }
                        ]
                    }
                )
                write(f"data: {chunk}\n\n".encode())
                time.sleep(delay_s)
            write(b"data: [DONE]\n\n")
        else:
            self.send_response(404)
            self.end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else PORT
    server = http.server.HTTPServer(("127.0.0.1", port), Handler)
    print(f"[fixture] listening on 127.0.0.1:{port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("[fixture] stopped")
