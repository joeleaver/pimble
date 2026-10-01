"""A minimal MCP client over stdio for walking pimble-mcp through its tools.

    python3 drive.py <pimble-mcp binary> <steps.json>

steps.json is a list of [tool, {args}] pairs. Every stdout line must be a JSON
message (anything else fails the run: stdout carries the protocol only).
"""
import json
import subprocess
import sys
import threading
import time

binary, steps_file = sys.argv[1], sys.argv[2]
steps = json.load(open(steps_file))

proc = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(steps_file + ".stderr", "w"), text=True, bufsize=1)
next_id = 0


def send(obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def request(method, params=None):
    global next_id
    next_id += 1
    send({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params or {}})
    while True:
        line = proc.stdout.readline()
        if not line:
            raise SystemExit(f"pimble-mcp closed stdout (exit {proc.poll()})")
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            raise SystemExit(f"NON-PROTOCOL LINE ON STDOUT: {line!r}")
        if msg.get("id") == next_id:
            return msg


started = time.time()
init = request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "drive", "version": "0"}})
print("server:", init["result"]["serverInfo"], f"({time.time() - started:.1f}s)")
send({"jsonrpc": "2.0", "method": "notifications/initialized"})
tools = request("tools/list")["result"]["tools"]
print("tools:", ", ".join(t["name"] for t in tools))

for tool, args in steps:
    if tool == "_wait_file":
        print(f"\n... waiting for {args['path']}", flush=True)
        while not __import__("os").path.exists(args["path"]):
            time.sleep(0.2)
        continue
    if tool == "_sleep":
        time.sleep(args["s"])
        continue
    t = time.time()
    reply = request("tools/call", {"name": tool, "arguments": args})
    result = reply.get("result", {})
    text = "\n".join(c.get("text", "") for c in result.get("content", []))
    flag = "ERROR " if result.get("isError") or "error" in reply else ""
    if "error" in reply:
        text = json.dumps(reply["error"])
    print(f"\n=== {flag}{tool} {json.dumps(args)[:160]} ({(time.time() - t) * 1000:.0f} ms)\n{text}", flush=True)

proc.stdin.close()
try:
    proc.wait(timeout=20)
except subprocess.TimeoutExpired:
    proc.kill()
print(f"\nexit {proc.returncode}")
