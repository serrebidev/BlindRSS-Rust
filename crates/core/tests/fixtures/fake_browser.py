#!/usr/bin/env python3
"""Fake Chromium for BlindRSS browser tests (Audit 8 Task 24).

Speaks just enough of the DevTools protocol for the Rust
browser stage: HTTP discovery (/json/list), one websocket
page target, Page.navigate, Runtime.evaluate page-state
reads, a Turnstile click target, Network.getCookies, and
the tier-4 turnstile.render hook surface
(window.__blindrssTurnstile / __blindrssTurnstileCallback).

Behaviour is driven by a JSON file named by the
BLINDRSS_FAKE_BROWSER_BEHAVIOR environment variable:

{
  "log": "<path appended with LAUNCH/NAVIGATE/CLICK/TOKEN lines>",
  "user_agent": "FakeBrowserUA/1.0",
  "pages": {
    "<url>": {
      "html": "<page source served for the URL>",
      "after_html": "<page source after a widget click>",
      "clear_on_click": true,
      "solved_html": "<page source after token delivery>",
      "turnstile": {"sitekey": "...", "data": "...",
                    "pagedata": "...", "action": "...",
                    "userAgent": "..."},
      "cookies": [{"name": "...", "value": "...",
                   "domain": "...", "path": "/",
                   "secure": false, "httpOnly": false,
                   "expires": 0}]
    }
  }
}

The process runs until killed, like a real browser.
"""

import base64
import hashlib
import json
import os
import struct
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def main():
    args = sys.argv[1:]
    user_data = ""
    port = 0
    for arg in args:
        if arg.startswith("--user-data-dir="):
            user_data = arg.split("=", 1)[1]
        elif arg.startswith("--remote-debugging-port="):
            port = int(arg.split("=", 1)[1])

    with open(os.environ["BLINDRSS_FAKE_BROWSER_BEHAVIOR"]) as handle:
        behavior = json.load(handle)
    log_path = behavior["log"]
    pages = behavior.get("pages", {})
    log_lock = threading.Lock()

    def log(line):
        with log_lock:
            with open(log_path, "a") as handle:
                handle.write(line + "\n")

    log("LAUNCH " + " ".join(args))

    state = {
        "url": "about:blank",
        "hook": False,
        "clicked": False,
        "solved": False,
        "params_ready": False,
    }

    def page():
        return pages.get(state["url"], {})

    def current_html():
        current = page()
        if state["solved"] and current.get("solved_html") is not None:
            return current["solved_html"]
        if state["clicked"] and current.get("after_html") is not None:
            return current["after_html"]
        return current.get("html", "")

    def evaluate(expression):
        """Return (type, value) for a Runtime.evaluate call."""
        if "document.documentElement.outerHTML" in expression:
            return "string", json.dumps(
                {"html": current_html(), "url": state["url"]}
            )
        if "__blindrssTurnstileCallback" in expression:
            # (function(t){ ... })("<token>") — the token is
            # the final parenthesised JSON string literal.
            tail = expression.rsplit(")(", 1)[-1].rstrip()
            if tail.endswith(")"):
                tail = tail[:-1]
            try:
                token = json.loads(tail)
            except Exception:
                token = ""
            log("TOKEN " + str(token))
            state["solved"] = True
            return "undefined", None
        if "__blindrssTurnstile" in expression:
            if state["params_ready"]:
                params = dict(page().get("turnstile", {}))
                params["pageurl"] = state["url"]
                return "object", params
            return "undefined", None
        if "getBoundingClientRect" in expression:
            if page().get("turnstile") or page().get("clear_on_click"):
                return "string", json.dumps({"x": 10, "y": 10})
            return "undefined", None
        if expression.strip() == "navigator.userAgent":
            return "string", behavior.get("user_agent", "FakeBrowserUA/1.0")
        return "undefined", None

    def dispatch(message):
        method = message.get("method", "")
        params = message.get("params") or {}
        if method == "Runtime.evaluate":
            value_type, value = evaluate(params.get("expression", ""))
            return {"result": {"type": value_type, "value": value}}
        if method == "Page.navigate":
            state["url"] = params.get("url", "")
            state["clicked"] = False
            state["solved"] = False
            state["params_ready"] = bool(
                state["hook"] and page().get("turnstile")
            )
            log("NAVIGATE " + state["url"])
            return {"frameId": "frame-1", "loaderId": "loader-1"}
        if method == "Page.addScriptToEvaluateOnNewDocument":
            state["hook"] = True
            return {"identifier": "hook-1"}
        if method == "Page.removeScriptToEvaluateOnNewDocument":
            state["hook"] = False
            return {}
        if method == "Input.dispatchMouseEvent":
            if params.get("type") == "mousePressed":
                log("CLICK")
                if page().get("clear_on_click"):
                    state["clicked"] = True
            return {}
        if method == "Network.getCookies":
            return {"cookies": page().get("cookies", [])}
        return {}

    def read_exact(stream, count):
        data = b""
        while len(data) < count:
            chunk = stream.read(count - len(data))
            if not chunk:
                raise ConnectionError("socket closed")
            data += chunk
        return data

    def send_frame(writer, payload):
        data = payload.encode("utf-8")
        header = bytearray([0x81])
        if len(data) < 126:
            header.append(len(data))
        elif len(data) < 65536:
            header.append(126)
            header.extend(struct.pack(">H", len(data)))
        else:
            header.append(127)
            header.extend(struct.pack(">Q", len(data)))
        writer.write(bytes(header) + data)
        writer.flush()

    def websocket_loop(reader, writer):
        try:
            while True:
                first = read_exact(reader, 2)
                opcode = first[0] & 0x0F
                masked = first[1] & 0x80
                length = first[1] & 0x7F
                if length == 126:
                    length = struct.unpack(">H", read_exact(reader, 2))[0]
                elif length == 127:
                    length = struct.unpack(">Q", read_exact(reader, 8))[0]
                mask = read_exact(reader, 4) if masked else b"\x00" * 4
                payload = bytearray(read_exact(reader, length))
                for i in range(length):
                    payload[i] ^= mask[i % 4]
                if opcode == 8:
                    send_frame(writer, "")
                    return
                if opcode == 9:
                    continue
                if opcode != 1:
                    continue
                message = json.loads(bytes(payload).decode("utf-8"))
                result = dispatch(message)
                send_frame(
                    writer,
                    json.dumps({"id": message.get("id"), "result": result}),
                )
        except Exception:
            return

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_GET(self):
            server_port = self.server.server_address[1]
            if self.path.startswith("/devtools/page"):
                key = self.headers.get("Sec-WebSocket-Key", "")
                accept = base64.b64encode(
                    hashlib.sha1((key + WS_GUID).encode()).digest()
                ).decode()
                self.send_response(101)
                self.send_header("Upgrade", "websocket")
                self.send_header("Connection", "Upgrade")
                self.send_header("Sec-WebSocket-Accept", accept)
                self.end_headers()
                self.close_connection = True
                websocket_loop(self.rfile, self.wfile)
                return
            if self.path.startswith("/json/list"):
                body = json.dumps(
                    [
                        {
                            "id": "page-1",
                            "type": "page",
                            "url": state["url"],
                            "webSocketDebuggerUrl": (
                                f"ws://127.0.0.1:{server_port}/devtools/page/1"
                            ),
                        }
                    ]
                ).encode()
            elif self.path.startswith("/json/version"):
                body = json.dumps(
                    {
                        "Browser": "FakeBrowser/1.0",
                        "webSocketDebuggerUrl": (
                            f"ws://127.0.0.1:{server_port}/devtools/browser/1"
                        ),
                    }
                ).encode()
            else:
                body = b"{}"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    actual_port = server.server_address[1]
    if user_data:
        try:
            with open(os.path.join(user_data, "DevToolsActivePort"), "w") as f:
                f.write(f"{actual_port}\n/devtools/browser/fake\n")
        except OSError:
            pass
    server.serve_forever()


if __name__ == "__main__":
    main()
