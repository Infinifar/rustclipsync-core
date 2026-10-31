#!/usr/bin/env python3
"""Run against an isolated local server/database, never a deployed service."""
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 tests/server_e2e.py /path/to/rustclipsync-server")
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    project = Path(__file__).resolve().parents[1]
    subprocess.run(["cargo", "build", "--locked", "--example", "protocol_probe"], cwd=project, check=True)
    metadata = subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=project)
    import json
    target = Path(json.loads(metadata)["target_directory"])
    probe = target / "debug/examples" / ("protocol_probe.exe" if os.name == "nt" else "protocol_probe")
    with tempfile.TemporaryDirectory(prefix="rcs-core-server-") as tmp:
        root = Path(tmp)
        backend = free_port()
        env = os.environ.copy()
        env.update({"RCS_USER": "fixture", "RCS_PASSWORD": "integration-password-only", "RCS_DB_KEY": "2a" * 32,
                    "RCS_DATA_DIR": str(root / "data"), "RCS_LISTEN": f"127.0.0.1:{backend}"})
        server = subprocess.Popen([binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        proxy = None
        try:
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError("fixture server exited")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{backend}/healthz", timeout=1) as r:
                        if r.status == 200:
                            break
                except OSError:
                    time.sleep(.1)
            else:
                raise RuntimeError("fixture server startup timeout")
            cert, key = root / "cert.pem", root / "key.pem"
            subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
                            "-keyout", str(key), "-out", str(cert)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

            class Proxy(BaseHTTPRequestHandler):
                protocol_version = "HTTP/1.1"

                def log_message(self, *args):
                    pass

                def forward(self):
                    connection = http.client.HTTPConnection("127.0.0.1", backend, timeout=32)
                    try:
                        length = int(self.headers.get("Content-Length", "0"))
                        if not 0 <= length <= 10 * 1024 * 1024 + 4096:
                            self.send_error(413)
                            return
                        body = self.rfile.read(length) if length else None
                        headers = {k: v for k, v in self.headers.items() if k.lower() not in {"host", "connection"}}
                        connection.request(self.command, self.path, body=body, headers=headers)
                        response = connection.getresponse()
                        self.send_response(response.status)
                        for name, value in response.getheaders():
                            if name.lower() not in {"connection", "transfer-encoding", "server", "date"}:
                                self.send_header(name, value)
                        self.end_headers()
                        while chunk := response.read(65536):
                            self.wfile.write(chunk)
                    finally:
                        connection.close()

                do_GET = do_POST = do_PUT = do_DELETE = forward

            proxy = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
            proxy.daemon_threads = True
            tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            tls.load_cert_chain(cert, key)
            proxy.socket = tls.wrap_socket(proxy.socket, server_side=True)
            threading.Thread(target=proxy.serve_forever, daemon=True).start()
            env["RCS_CORE_DISPOSABLE_TEST"] = "1"
            subprocess.run([str(probe), f"https://127.0.0.1:{proxy.server_port}"], env=env, check=True, timeout=90)
        finally:
            if proxy:
                proxy.shutdown()
                proxy.server_close()
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    main()
