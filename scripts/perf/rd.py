"""A client for rinch's debug port (the `debug` feature pimble-app builds with).

    python3 rd.py <pid> <method> [json-params] [screenshot.png]

`pid` is a running pimble's process id: rinch writes `~/.rinch/debug/<pid>.json`
naming the port. Methods and params are rinch-debug's (`crates/rinch-debug/src/
protocol.rs` in rinch): screenshot, click {x, y}, type_text {text}, key_press
{key, shift, ctrl}, scroll {x, y, delta_x, delta_y}, wait_frame, dom_tree, ...

    python3 rd.py 12345 click '{"x": 25, "y": 213}'
    python3 rd.py 12345 screenshot '' shot.png
"""
import base64
import json
import os
import socket
import struct
import sys
import time


def _frame(sock, obj):
    body = json.dumps(obj).encode()
    sock.sendall(struct.pack(">I", len(body)) + body)


def _read(sock):
    head = b""
    while len(head) < 4:
        head += sock.recv(4 - len(head))
    n = struct.unpack(">I", head)[0]
    body = b""
    while len(body) < n:
        body += sock.recv(n - len(body))
    return json.loads(body)


class Client:
    def __init__(self, pid):
        entry = json.load(open(os.path.expanduser(f"~/.rinch/debug/{pid}.json")))
        self.sock = socket.create_connection(("127.0.0.1", entry["port"]))
        _frame(self.sock, {"protocol": "rinch-debug", "version": 1})
        _read(self.sock)
        self.next_id = 0

    def call(self, method, params=None):
        self.next_id += 1
        request = {"id": self.next_id, "method": method}
        if params is not None:
            request["params"] = params
        _frame(self.sock, request)
        return _read(self.sock)


if __name__ == "__main__":
    client = Client(sys.argv[1])
    params = json.loads(sys.argv[3]) if len(sys.argv) > 3 and sys.argv[3] else None
    started = time.time()
    reply = client.call(sys.argv[2], params)
    elapsed = (time.time() - started) * 1000
    if reply.get("type") == "bytes":
        out = sys.argv[4] if len(sys.argv) > 4 else "shot.png"
        open(out, "wb").write(base64.b64decode(reply["data"]))
        print(f"wrote {out} ({elapsed:.0f} ms)")
    else:
        print(json.dumps(reply)[:4000])
        print(f"({elapsed:.0f} ms)", file=sys.stderr)
