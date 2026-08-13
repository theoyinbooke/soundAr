from __future__ import annotations

import json
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from pathlib import Path

from soundar_cli import load_batch_rows, request


class ApiHandler(BaseHTTPRequestHandler):
    authorization = ""
    idempotency_key = ""
    body = b""

    def do_GET(self) -> None:
        body = json.dumps({"status": "ready", "local_only": True}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        type(self).authorization = self.headers.get("Authorization", "")
        type(self).idempotency_key = self.headers.get("Idempotency-Key", "")
        type(self).body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        body = json.dumps({"created": True}).encode()
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        pass


class SoundarCliTests(unittest.TestCase):
    def test_batch_loader_preserves_csv_and_jsonl_row_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            csv_path = Path(directory) / "batch.csv"
            csv_path.write_text('name,text,language,speed,output_name,priority\nIntro,"Hello, listener.",en,0.9,Opening Clip,urgent\n', encoding="utf-8")
            csv_rows = load_batch_rows(csv_path)
            self.assertEqual(csv_rows[0]["text"], "Hello, listener.")
            self.assertEqual(csv_rows[0]["settings"]["speed"], 0.9)
            self.assertEqual(csv_rows[0]["output_name"], "Opening Clip")
            self.assertEqual(csv_rows[0]["priority"], "urgent")

            jsonl_path = Path(directory) / "batch.jsonl"
            jsonl_path.write_text('{"text":"Bonjour","name":"French","settings":{"language":"fr","seed":9}}\n', encoding="utf-8")
            jsonl_rows = load_batch_rows(jsonl_path)
            self.assertEqual(jsonl_rows[0]["settings"]["seed"], 9)

    def test_batch_loader_rejects_malformed_rows(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "broken.jsonl"
            path.write_text("{broken}\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "row 1"):
                load_batch_rows(path)

    def test_request_sends_bearer_token_and_decodes_response(self) -> None:
        server = ThreadingHTTPServer(("127.0.0.1", 0), ApiHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            body, media_type = request(f"http://127.0.0.1:{server.server_port}", "test-token", "/health")
            self.assertEqual(json.loads(body)["status"], "ready")
            self.assertEqual(media_type, "application/json")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_request_posts_json_with_bearer_token(self) -> None:
        server = ThreadingHTTPServer(("127.0.0.1", 0), ApiHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            body, media_type = request(
                f"http://127.0.0.1:{server.server_port}",
                "post-token",
                "/v1/batches",
                {"name": "test", "scripts": ["one"]},
                {"Idempotency-Key": "stable-retry-key"},
            )
            self.assertTrue(json.loads(body)["created"])
            self.assertEqual(media_type, "application/json")
            self.assertEqual(ApiHandler.authorization, "Bearer post-token")
            self.assertEqual(ApiHandler.idempotency_key, "stable-retry-key")
            self.assertEqual(json.loads(ApiHandler.body)["scripts"], ["one"])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
