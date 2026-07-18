#!/usr/bin/env python3

from __future__ import annotations

import hmac
import http.client
import secrets
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener


STATIC_DIR = Path(__file__).resolve().parent
PROXY_TOKEN = secrets.token_urlsafe(32)
HOP_BY_HOP_HEADERS = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}


class SafeRedirectHandler(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        redirected = super().redirect_request(request, fp, code, message, headers, new_url)
        if redirected is None:
            return None

        old = urlsplit(request.full_url)
        new = urlsplit(redirected.full_url)
        if (old.scheme, old.hostname, old.port) != (new.scheme, new.hostname, new.port):
            redirected.remove_header("Authorization")
        return redirected


UPSTREAM = build_opener(SafeRedirectHandler())


class Handler(SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(STATIC_DIR), **kwargs)

    def do_GET(self):
        parsed = urlsplit(self.path)
        if parsed.path == "/healthz":
            self._send_bytes(200, b"ok\n", "text/plain; charset=utf-8")
            return
        if parsed.path == "/proxy-token":
            body = f"{PROXY_TOKEN}\n".encode()
            self._send_bytes(
                200,
                body,
                "text/plain; charset=utf-8",
                {"Cache-Control": "no-store"},
            )
            return
        if parsed.path == "/proxy":
            self._proxy(parsed.query)
            return
        super().do_GET()

    def do_OPTIONS(self):
        self.send_error(405, "Cross-origin proxy requests are not allowed")

    def end_headers(self):
        # This is a development server. Keeping the HTML, modules, worker, and
        # Wasm uncached prevents a browser tab from combining a newly rebuilt
        # UI with an older scanner module.
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        super().end_headers()

    def _proxy(self, query):
        supplied_token = self.headers.get("X-OCI-Zero-Proxy", "")
        if not hmac.compare_digest(supplied_token, PROXY_TOKEN):
            self._send_bytes(
                403,
                b"Invalid proxy token\n",
                "text/plain; charset=utf-8",
                {"X-OCI-Zero-Proxy-Error": "invalid-token"},
            )
            return

        targets = parse_qs(query, keep_blank_values=True).get("url", [])
        if len(targets) != 1:
            self.send_error(400, "Expected one url parameter")
            return
        target = targets[0]
        parsed = urlsplit(target)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname:
            self.send_error(400, "Proxy URLs must be absolute HTTP(S) URLs")
            return
        if parsed.username is not None or parsed.password is not None:
            self.send_error(400, "Proxy URLs must not contain credentials")
            return

        headers = {"User-Agent": "oci-zero-browser/0.1"}
        for name in ("Accept", "Authorization"):
            if value := self.headers.get(name):
                headers[name] = value

        request = Request(target, headers=headers, method="GET")
        try:
            response = UPSTREAM.open(request, timeout=60)
        except HTTPError as error:
            response = error
        except (URLError, TimeoutError, OSError, http.client.HTTPException) as error:
            self.send_error(502, f"Upstream request failed: {error}")
            return

        try:
            self.send_response(response.status)
            for name, value in response.headers.items():
                lowered = name.lower()
                if (
                    lowered not in HOP_BY_HOP_HEADERS
                    and lowered != "set-cookie"
                    and not lowered.startswith("access-control-")
                ):
                    self.send_header(name, value)
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            while chunk := response.read(64 * 1024):
                self.wfile.write(chunk)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            response.close()

    def _send_bytes(self, status, body, content_type, extra_headers=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in (extra_headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    server = ThreadingHTTPServer(("0.0.0.0", 8000), Handler)
    print("Serving OCI Zero Browser on http://0.0.0.0:8000", flush=True)
    server.serve_forever()
