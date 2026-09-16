#!/usr/bin/env python3
"""Drive a running rinch app (debug feature) over its TCP debug protocol.

Usage:
  rdbg.py [--pid PID] apps
  rdbg.py [--pid PID] screenshot OUT.png
  rdbg.py [--pid PID] dom [DEPTH]
  rdbg.py [--pid PID] query SELECTOR
  rdbg.py [--pid PID] node ID
  rdbg.py [--pid PID] text ID
  rdbg.py [--pid PID] click X Y
  rdbg.py [--pid PID] rclick X Y
  rdbg.py [--pid PID] type TEXT
  rdbg.py [--pid PID] key KEY [ctrl] [shift]
  rdbg.py [--pid PID] frame
  rdbg.py [--pid PID] raw JSON      (a full command object: {"method": ..., "params": ...})
"""
import base64, glob, json, os, socket, struct, sys

def apps():
    out = []
    for p in glob.glob(os.path.expanduser("~/.rinch/debug/*.json")):
        try:
            e = json.load(open(p))
        except Exception:
            continue
        if os.path.exists(f"/proc/{e['pid']}"):
            out.append(e)
    return out

def frame_write(s, obj):
    data = json.dumps(obj).encode()
    s.sendall(struct.pack(">I", len(data)) + data)

def frame_read(s):
    hdr = b""
    while len(hdr) < 4:
        chunk = s.recv(4 - len(hdr))
        if not chunk:
            raise RuntimeError("closed")
        hdr += chunk
    n = struct.unpack(">I", hdr)[0]
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise RuntimeError("closed")
        buf += chunk
    return json.loads(buf)

def connect(pid=None):
    entries = apps()
    if pid is not None:
        entries = [e for e in entries if e["pid"] == pid]
    if not entries:
        sys.exit("no rinch app with debug enabled is running")
    e = sorted(entries, key=lambda e: e["started_at"])[-1]
    s = socket.create_connection(("127.0.0.1", e["port"]), timeout=60)
    frame_write(s, {"protocol": "rinch-debug", "version": 1})
    frame_read(s)
    return s

def run(s, method, params=None, rid=1):
    req = {"id": rid, "method": method}
    if params is not None:
        req["params"] = params
    frame_write(s, req)
    return frame_read(s)

def main():
    args = sys.argv[1:]
    pid = None
    if args and args[0] == "--pid":
        pid = int(args[1]); args = args[2:]
    if not args:
        print(__doc__); return
    cmd, rest = args[0], args[1:]
    if cmd == "apps":
        for e in apps():
            print(json.dumps(e))
        return
    s = connect(pid)
    if cmd == "screenshot":
        r = run(s, "screenshot")
        if r.get("type") == "bytes":
            open(rest[0], "wb").write(base64.b64decode(r["data"]))
            print("wrote", rest[0])
        else:
            print(json.dumps(r))
    elif cmd == "dom":
        params = {"max_depth": int(rest[0])} if rest else {}
        r = run(s, "dom_tree", params)
        print(json.dumps(r.get("data", r), indent=1))
    elif cmd == "query":
        r = run(s, "query_selector", {"selector": rest[0]})
        print(json.dumps(r.get("data", r), indent=1))
    elif cmd == "node":
        r = run(s, "get_node", {"id": int(rest[0])})
        print(json.dumps(r.get("data", r), indent=1))
    elif cmd == "text":
        r = run(s, "get_text_content", {"id": int(rest[0])})
        print(json.dumps(r.get("data", r), indent=1))
    elif cmd in ("click", "rclick"):
        params = {"x": float(rest[0]), "y": float(rest[1])}
        if cmd == "rclick":
            params["button"] = "right"
        print(json.dumps(run(s, "click", params)))
    elif cmd == "type":
        print(json.dumps(run(s, "type_text", {"text": rest[0]})))
    elif cmd == "key":
        print(json.dumps(run(s, "key_press", {"key": rest[0], "ctrl": "ctrl" in rest[1:], "shift": "shift" in rest[1:]})))
    elif cmd == "frame":
        print(json.dumps(run(s, "wait_frame")))
    elif cmd == "raw":
        obj = json.loads(rest[0])
        print(json.dumps(run(s, obj["method"], obj.get("params")), indent=1))
    else:
        sys.exit(f"unknown command {cmd}")

if __name__ == "__main__":
    main()
