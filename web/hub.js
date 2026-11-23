// The browser's one Pimble server, shared by every tab of this origin
// (docs/ONE_CODEBASE_CONTRACT.md section 4): used only with the worker switch
// on (`?server=worker`, kept in localStorage `pimble.server`).
//
// Every tab asks for one Web Lock, `pimble-server`. The tab that holds it is
// the leader: it runs the server in a dedicated worker (the page's build
// writes it, `pimble-worker_loader.js`, from crates/pimble-worker with its
// `entry` feature) and relays the other tabs' connections to it. The others
// talk to it through a BroadcastChannel, JSON-RPC text addressed by a
// connection id. When the leader tab closes, the lock passes to the next tab,
// which starts its worker on the same OPFS files and says so; every tab's
// connections end and connect again, as the desktop app's do when the
// embedded server it borrowed goes away.
//
// What the page's Rust sees (web/src/worker_mode/hub.rs): `PimbleHub.start`,
// `PimbleHub.connect()` (a promise of a MessagePort whose other end reaches
// the server; `null` arriving on it says the connection ended:
// crates/pimble-client message_port.rs), `PimbleHub.control(cmd)` (restart
// in another mode, forget this browser) and `PimbleHub.status()`.
//
// Messages on the channel, each `{ t, ... }`:
//   who                     a tab asking who leads
//   leader { id, mode }     the leader, answering or newly in place
//   bye { id }              the leader tab is going
//   open { conn, from, to } a tab's new connection, for the leader `to`
//   rpc { conn, to?, text } text for one connection, either way
//   close { conn }          a tab's connection ended
//   end { conn }            the leader ended a tab's connection
//   control { req, from, cmd } / controlled { req, to, ok, value }
(function () {
    "use strict";

    const LOCK = "pimble-server";
    const CHANNEL = "pimble-server";
    const me = crypto.randomUUID();

    let options = { mode: "auto", inherited: null };
    let started = false;
    let bc = null;

    // Who leads, as this tab knows it: `{ id, mode }`, or null while nobody
    // has said.
    let leader = null;
    let leaderWaiters = [];

    // This tab's connections: conn id -> { hub: the hub's end of the
    // client's channel, worker?: the port to the worker (leader),
    // remote?: true (through the channel) }.
    const local = new Map();
    // The leader's relays for other tabs' connections: conn id -> { worker
    // port, tab }.
    const relayed = new Map();

    // The leader's worker.
    let worker = null;
    let workerMode = null;
    let workerReady = null;
    let controlWaiters = new Map();

    function log(...args) {
        console.info("[pimble hub]", ...args);
    }

    function setLeader(next) {
        const changed = !leader || !next || leader.id !== next.id;
        leader = next;
        if (next) {
            const waiting = leaderWaiters;
            leaderWaiters = [];
            waiting.forEach((resolve) => resolve(next));
        }
        return changed;
    }

    function knownLeader() {
        if (leader) return Promise.resolve(leader);
        return new Promise((resolve) => leaderWaiters.push(resolve));
    }

    // End one of this tab's connections: its client hears `null` and
    // connects again.
    function endLocal(conn) {
        const c = local.get(conn);
        if (!c) return;
        local.delete(conn);
        try { c.hub.postMessage(null); } catch (e) {}
        if (c.worker) {
            try { c.worker.postMessage(null); c.worker.close(); } catch (e) {}
        }
        if (c.remote && bc && leader && leader.id !== me) {
            bc.postMessage({ t: "close", conn });
        }
    }

    function endAllLocal(why) {
        if (local.size) log("ending this tab's connections:", why);
        for (const conn of Array.from(local.keys())) endLocal(conn);
    }

    function endRelayed(conn, tell) {
        const r = relayed.get(conn);
        if (!r) return;
        relayed.delete(conn);
        try { r.worker.postMessage(null); r.worker.close(); } catch (e) {}
        if (tell) bc.postMessage({ t: "end", conn });
    }

    function endAllRelayed() {
        for (const conn of Array.from(relayed.keys())) endRelayed(conn, true);
    }

    // ---- The leader's worker ----

    function workerUrl() {
        // The page is served under /app/ (Trunk.toml public_url).
        return new URL("/app/pimble-worker_loader.js", location.origin).href;
    }

    function startWorker(mode) {
        workerReady = new Promise((resolve, reject) => {
            const started = performance.now();
            const w = new Worker(workerUrl());
            worker = w;
            w.onmessage = (event) => {
                const m = event.data || {};
                if (m.t === "loaded") {
                    log("worker loaded in", Math.round(performance.now() - started), "ms; starting the server:", mode);
                    w.postMessage({ t: "start", mode, inherited: options.inherited });
                } else if (m.t === "started") {
                    workerMode = m.mode;
                    log("server started:", m.mode, "in", Math.round(performance.now() - started), "ms from the worker's start");
                    resolve(m.mode);
                } else if (m.t === "failed") {
                    log("the worker failed:", m.message);
                    const waiter = controlWaiters.get("worker");
                    if (waiter) { controlWaiters.delete("worker"); waiter.reject(new Error(m.message)); }
                    reject(new Error(m.message));
                } else if (m.t === "stopped" || m.t === "forgotten") {
                    const waiter = controlWaiters.get("worker");
                    if (waiter) { controlWaiters.delete("worker"); waiter.resolve(m); }
                }
            };
            w.onerror = (event) => {
                log("the worker failed:", event.message || event);
                if (worker === w) restartWorker(workerMode === "remembered" ? "remember" : "memory", "the worker failed");
            };
        });
        workerReady.catch(() => {});
        return workerReady;
    }

    function askWorker(message) {
        return new Promise((resolve, reject) => {
            controlWaiters.set("worker", { resolve, reject });
            worker.postMessage(message);
        });
    }

    async function restartWorker(mode, why) {
        log("restarting the server:", why, "->", mode);
        const old = worker;
        worker = null;
        if (old) old.terminate();
        endAllLocal(why);
        endAllRelayed();
        await startWorker(mode);
        announce();
    }

    function announce() {
        const next = { id: me, mode: workerMode };
        setLeader(next);
        bc.postMessage({ t: "leader", id: me, mode: workerMode });
    }

    async function becomeLeader() {
        log("this tab leads; starting the server");
        // Connections this tab had through the old leader are gone with it.
        endAllLocal("this tab leads now");
        try {
            await startWorker(options.mode);
        } catch (e) {
            log("the server did not start:", e.message);
        }
        announce();
        // Stop cleanly when this tab goes, if it is given the moment.
        addEventListener("pagehide", () => {
            bc.postMessage({ t: "bye", id: me });
            if (worker) worker.postMessage({ t: "stop" });
        });
        // Held for as long as this tab lives.
        return new Promise(() => {});
    }

    // A connection of this tab's straight to its own worker.
    function connectToWorker(conn, hub) {
        const channel = new MessageChannel();
        worker.postMessage({ t: "connect" }, [channel.port2]);
        const c = { hub, worker: channel.port1 };
        local.set(conn, c);
        hub.onmessage = (event) => {
            if (event.data === null) { endLocal(conn); return; }
            channel.port1.postMessage(event.data);
        };
        channel.port1.onmessage = (event) => {
            if (event.data === null) { endLocal(conn); return; }
            hub.postMessage(event.data);
        };
    }

    // ---- The channel ----

    function onBroadcast(event) {
        const m = event.data || {};
        const leading = leader && leader.id === me;
        switch (m.t) {
            case "who":
                if (leading) bc.postMessage({ t: "leader", id: me, mode: workerMode });
                break;
            case "leader":
                if (m.id !== me) {
                    const changed = setLeader({ id: m.id, mode: m.mode });
                    if (changed) endAllLocal("another tab leads now");
                    else leader.mode = m.mode;
                }
                break;
            case "bye":
                if (leader && leader.id === m.id) {
                    setLeader(null);
                    endAllLocal("the leading tab closed");
                }
                break;
            case "open":
                if (leading && m.to === me && worker) {
                    const channel = new MessageChannel();
                    worker.postMessage({ t: "connect" }, [channel.port2]);
                    relayed.set(m.conn, { worker: channel.port1, tab: m.from });
                    channel.port1.onmessage = (e) => {
                        if (e.data === null) { endRelayed(m.conn, true); return; }
                        bc.postMessage({ t: "rpc", conn: m.conn, text: e.data });
                    };
                } else if (leading && m.to === me) {
                    bc.postMessage({ t: "end", conn: m.conn });
                }
                break;
            case "rpc": {
                const r = relayed.get(m.conn);
                if (r && leading) { r.worker.postMessage(m.text); break; }
                const c = local.get(m.conn);
                if (c && c.remote) c.hub.postMessage(m.text);
                break;
            }
            case "close":
                if (leading) endRelayed(m.conn, false);
                break;
            case "end":
                if (local.has(m.conn)) endLocal(m.conn);
                break;
            case "control":
                if (leading) {
                    runControl(m.cmd).then(
                        (value) => bc.postMessage({ t: "controlled", req: m.req, to: m.from, ok: true, value }),
                        (e) => bc.postMessage({ t: "controlled", req: m.req, to: m.from, ok: false, value: String(e && e.message || e) }),
                    );
                }
                break;
            case "controlled":
                if (m.to === me) {
                    const waiter = controlWaiters.get(m.req);
                    if (waiter) {
                        controlWaiters.delete(m.req);
                        if (m.ok) waiter.resolve(m.value); else waiter.reject(new Error(m.value));
                    }
                }
                break;
        }
    }

    // What only the leader does to its worker.
    async function runControl(cmd) {
        await workerReady.catch(() => {});
        if (cmd.t === "restart") {
            if (workerMode === (cmd.mode === "remember" ? "remembered" : "memory")) return workerMode;
            if (worker) await askWorker({ t: "stop" }).catch(() => {});
            await restartWorker(cmd.mode, "asked to keep its stores " + (cmd.mode === "remember" ? "in this browser" : "in memory"));
            return workerMode;
        }
        if (cmd.t === "forget") {
            const answer = worker ? await askWorker({ t: "forget" }) : { empty: true };
            await restartWorker("memory", "this browser was forgotten");
            return { empty: !!answer.empty };
        }
        throw new Error("unknown control " + cmd.t);
    }

    // ---- What the page calls ----

    function start(given) {
        if (started) return;
        started = true;
        options = Object.assign(options, given || {});
        bc = new BroadcastChannel(CHANNEL);
        bc.onmessage = onBroadcast;
        if (!navigator.locks) {
            log("this browser has no Web Locks; this tab runs its own server");
            becomeLeader();
            return;
        }
        navigator.locks.request(LOCK, () => {
            // The lock is ours: whoever led before is gone.
            return becomeLeader();
        });
        bc.postMessage({ t: "who" });
    }

    async function connect() {
        const l = await knownLeader();
        const channel = new MessageChannel();
        const conn = crypto.randomUUID();
        if (l.id === me) {
            await workerReady.catch(() => {});
            if (!worker) throw new Error("the server is not running");
            connectToWorker(conn, channel.port2);
        } else {
            local.set(conn, { hub: channel.port2, remote: true });
            channel.port2.onmessage = (event) => {
                if (event.data === null) { endLocal(conn); return; }
                bc.postMessage({ t: "rpc", conn, text: event.data });
            };
            bc.postMessage({ t: "open", conn, from: me, to: l.id });
        }
        return channel.port1;
    }

    async function control(cmd) {
        const l = await knownLeader();
        if (l.id === me) return runControl(cmd);
        const req = crypto.randomUUID();
        return new Promise((resolve, reject) => {
            controlWaiters.set(req, { resolve, reject });
            bc.postMessage({ t: "control", req, from: me, cmd });
        });
    }

    // A Web Lock held until the answered function is called: a turn at
    // something one tab does at a time.
    function take(name) {
        if (!navigator.locks) return Promise.resolve(() => {});
        return new Promise((granted) => {
            navigator.locks.request(name, () => new Promise((release) => granted(() => release())));
        });
    }

    function status() {
        return {
            tab: me,
            leading: !!leader && leader.id === me,
            leader: leader ? leader.id : null,
            mode: leader ? leader.mode : null,
            connections: local.size,
            relayed: relayed.size,
        };
    }

    window.PimbleHub = { start, connect, control, status, take };
})();
