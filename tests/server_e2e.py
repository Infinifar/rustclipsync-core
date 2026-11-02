#!/usr/bin/env python3
"""Run against an isolated local server/database, never a deployed service."""
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import socket
import select
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
    for allow_ws in (True, False):
        run_once(binary, probe, allow_ws)


def run_once(binary, probe, allow_ws):
    with tempfile.TemporaryDirectory(prefix="rcs-core-server-") as tmp:
        root = Path(tmp)
        backend = free_port()
        env = {k: v for k, v in os.environ.items() if not k.startswith("RCS_")}
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

            counts = {"websocket": 0, "blocked": 0, "http_updates": 0}
            class Proxy(BaseHTTPRequestHandler):
                protocol_version = "HTTP/1.1"

                def log_message(self, *args):
                    pass

                def forward(self):
                    if self.command == "PUT" and self.path.endswith("/sync-direction"):
                        counts["http_updates"] += 1
                    upgrade = self.headers.get("Upgrade", "").lower() == "websocket"
                    if upgrade and not allow_ws:
                        counts["blocked"] += 1
                        self.send_response(403)
                        self.send_header("Content-Length", "0")
                        self.send_header("X-RCS-Protocol", "3")
                        self.end_headers()
                        return
                    connection = http.client.HTTPConnection("127.0.0.1", backend, timeout=32)
                    try:
                        length = int(self.headers.get("Content-Length", "0"))
                        if not 0 <= length <= 10 * 1024 * 1024 + 4096:
                            self.send_error(413)
                            return
                        body = self.rfile.read(length) if length else None
                        headers = {k: v for k, v in self.headers.items() if k.lower() not in {"host", "connection"}}
                        if upgrade:
                            headers["Connection"] = "Upgrade"
                        connection.request(self.command, self.path, body=body, headers=headers)
                        response = connection.getresponse()
                        if response.status == 101:
                            counts["websocket"] += 1
                        self.send_response(response.status)
                        for name, value in response.getheaders():
                            if name.lower() not in {"transfer-encoding", "server", "date"} and (upgrade or name.lower() != "connection"):
                                self.send_header(name, value)
                        self.end_headers()
                        if response.status == 101:
                            self.wfile.flush()
                            upstream = connection.sock
                            peers = [self.connection, upstream]
                            while True:
                                ready, _, _ = select.select(peers, [], [], 35)
                                if not ready:
                                    break
                                for source in ready:
                                    chunk = source.recv(65536)
                                    if not chunk:
                                        return
                                    destination = upstream if source is self.connection else self.connection
                                    destination.sendall(chunk)
                            return
                        while chunk := response.read(65536):
                            self.wfile.write(chunk)
                    except (BrokenPipeError, ConnectionResetError, ssl.SSLError, TimeoutError):
                        pass
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
            subprocess.run([str(probe), f"https://127.0.0.1:{proxy.server_port}"], env=env, check=True, timeout=120)
            if allow_ws:
                assert counts["websocket"] > 0, "probe never established the control WebSocket"
                assert counts["http_updates"] == 0, "WS updates unexpectedly fell back to HTTP"
            else:
                assert counts["blocked"] > 0 and counts["websocket"] == 0, "HTTP fallback was not exercised"
                assert counts["http_updates"] > 0, "no HTTP direction update was accepted"
            print("PASS: direction control via " + ("real WSS" if allow_ws else "HTTP fallback after blocked WSS"))
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
