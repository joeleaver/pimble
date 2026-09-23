"""Drive a running pimble through steps and summarise rinch's frame timings per step.

    python3 scenario.py <pid> <app.log> '<json steps>'

The app must run with `RINCH_PERF=1`, its stderr in `<app.log>`. Steps:

    ["click", x, y, "label"]      ["type", "text"]      ["key", "Enter"]
    ["scroll", x, y, delta_y]     ["shot", "out.png"]

Each step prints `ui-busy` (wall time until the app answered `wait_frame`, which
includes this driver's round trips: an upper bound, not UI-thread CPU; measure
that per thread id from /proc, see README) and the sums of rinch's per-frame
`[PERF]` lines that the step caused.
"""
import base64
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rd import Client  # noqa: E402

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def _sum(lines, pattern):
    return [float(m.group(1)) for line in lines if (m := re.search(pattern, line))]


def summarise(lines):
    resolve = _sum(lines, r"\] resolve: ([\d.]+)ms")
    # Software frames log "paint (software)", GPU frames "] paint:".
    paint = _sum(lines, r"(?:paint \(software\)|\] paint): ([\d.]+)ms")
    raster = _sum(lines, r"paint \((?:full|dirty[^)]*)\): ([\d.]+)ms")
    layout = _sum(lines, r"resolve_layout TOTAL: ([\d.]+)ms")
    ifc = _sum(lines, r"build_ifc: ([\d.]+)ms")
    return (
        f"frames={len(paint)} resolve={sum(resolve):.0f}ms (max {max(resolve, default=0):.0f}) "
        f"layout={sum(layout):.0f}ms build_ifc={sum(ifc):.0f}ms raster={sum(raster):.0f}ms "
        f"paint+present={sum(paint):.0f}ms (max {max(paint, default=0):.0f})"
    )


def main():
    pid, log_path, steps = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
    client = Client(pid)

    def log_lines():
        return open(log_path, errors="replace").readlines()

    def step(name, action, settle=1.5):
        start = len(log_lines())
        began = time.time()
        action()
        client.call("wait_frame")
        busy = (time.time() - began) * 1000
        time.sleep(settle)
        lines = [ANSI.sub("", line) for line in log_lines()[start:]]
        print(f"{name:34s} ui-busy={busy:6.0f}ms  {summarise(lines)}", flush=True)

    for s in steps:
        kind = s[0]
        if kind == "click":
            step(f"click {s[1]},{s[2]} {s[3] if len(s) > 3 else ''}",
                 lambda: client.call("click", {"x": s[1], "y": s[2]}))
        elif kind == "type":
            step(f"type {len(s[1])} chars",
                 lambda: [client.call("type_text", {"text": ch}) for ch in s[1]])
        elif kind == "key":
            step(f"key {s[1]}",
                 lambda: client.call("key_press", {"key": s[1], "shift": False, "ctrl": False}))
        elif kind == "scroll":
            step(f"scroll {s[3]}",
                 lambda: client.call("scroll", {"x": s[1], "y": s[2], "delta_x": 0, "delta_y": s[3]}))
        elif kind == "shot":
            reply = client.call("screenshot")
            open(s[1], "wb").write(base64.b64decode(reply["data"]))


if __name__ == "__main__":
    main()
