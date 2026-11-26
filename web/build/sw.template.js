// Pimble's service worker (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 4",
// the follow-ups): keeps this build's own files so the page opens with no
// network and runs on the browser's own server from OPFS. Registered by
// hub.js.
//
// Written by build/post_build.sh from build/sw.template.js: BUILD is this
// build's id and FILES its files, every one of them, and nothing else. It
// never answers for /api or /rpc (not under the app's path) and never keeps
// a response it did not list: no store data, no account data.
//
// A page is always this build's or the newest the network gives: the page
// itself (a navigation) is asked of the network first and taken from the
// cache only when the network cannot answer; its files are named by their
// contents, so a new page never finds an old file under its name. A new build
// installs its own cache whole, takes over at once and deletes the older
// ones, and tells the pages it took over from that a new version is ready.
const BUILD = "__PIMBLE_BUILD__";
const BASE = "__PIMBLE_BASE__";
const CACHE = "pimble-app-" + BUILD;
const FILES = ["__PIMBLE_FILES__"];
const INDEX = BASE + "index.html";
// How long a navigation waits for the network before the kept page.
const NETWORK_WAIT_MS = 4000;

self.addEventListener("install", (event) => {
    event.waitUntil(
        caches.open(CACHE)
            .then((cache) => cache.addAll(FILES.map((f) => new Request(f, { cache: "reload" }))))
            .then(() => self.skipWaiting()),
    );
});

self.addEventListener("activate", (event) => {
    event.waitUntil((async () => {
        let replaced = false;
        for (const name of await caches.keys()) {
            if (name.startsWith("pimble-app-") && name !== CACHE) {
                await caches.delete(name);
                replaced = true;
            }
        }
        await self.clients.claim();
        if (replaced) {
            for (const client of await self.clients.matchAll({ type: "window" })) {
                client.postMessage({ t: "pimble-updated", build: BUILD });
            }
        }
    })());
});

function ours(url) {
    return url.origin === self.location.origin && (url.pathname + "/").startsWith(BASE);
}

async function fromNetwork(request) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), NETWORK_WAIT_MS);
    try {
        return await fetch(request, { signal: controller.signal });
    } finally {
        clearTimeout(timer);
    }
}

self.addEventListener("fetch", (event) => {
    const request = event.request;
    if (request.method !== "GET") return;
    const url = new URL(request.url);
    if (!ours(url)) return;
    if (request.mode === "navigate") {
        // Every path under the app is the one page.
        event.respondWith(fromNetwork(request).catch(async () => (await caches.match(INDEX, { cacheName: CACHE })) || Response.error()));
        return;
    }
    event.respondWith((async () => (await caches.match(request, { cacheName: CACHE, ignoreSearch: true })) || fetch(request))());
});

self.addEventListener("message", (event) => {
    if (event.data && event.data.t === "pimble-build") event.source.postMessage({ t: "pimble-build", build: BUILD });
});

// ---- Web push (docs/NOTIFICATIONS_CONTRACT.md, wave 2) ----
//
// The accounts service pushes `{ v, id, from, to, kind, line, seen, sealed? }`
// (RFC 8291, opened by the browser before it reaches here). `sealed` is the
// notice sealed to the account's notice key, which the browser's own server
// keeps in IndexedDB as a non-extractable CryptoKey (`pimble-device`, `keys`,
// `notice`): opened here with notice.js (concatenated in front of this file),
// it shows who and the words around the mention, with no page open and no
// account unlocked. Without that key (a browser that does not remember
// itself, another account signed in here, a browser without X25519 in
// WebCrypto), or for a push that carries no sealed notice, `line` is shown:
// who and in which share, what an email says. Then the service is told the
// push was shown (`seen`), so it is not emailed too.

function noticeRecord() {
    return new Promise((resolve) => {
        let request;
        try {
            request = indexedDB.open("pimble-device", 1);
        } catch (e) {
            resolve(null);
            return;
        }
        // Not there: never make it here.
        request.onupgradeneeded = () => request.transaction.abort();
        request.onerror = () => resolve(null);
        request.onsuccess = () => {
            const db = request.result;
            try {
                const get = db.transaction("keys", "readonly").objectStore("keys").get("notice");
                get.onsuccess = () => { db.close(); resolve(get.result || null); };
                get.onerror = () => { db.close(); resolve(null); };
            } catch (e) {
                db.close();
                resolve(null);
            }
        };
    });
}

function titleOf(notice) {
    const who = (notice.from_name || "").trim() || "Someone";
    if (notice.kind === "reply") return who + " replied in a discussion";
    // Wave 3: as `pimble_core::watch::watch_title` says it (spells of
    // editing, not appends).
    if (notice.kind === "watch") {
        const n = notice.count || 1;
        return who + " edited a note you watch" + (n <= 1 ? "" : " " + n + " times");
    }
    return notice.discussion ? who + " mentioned you in a discussion" : who + " mentioned you";
}

async function openedNotice(push) {
    if (!push.sealed || !self.pimbleNotice) return null;
    try {
        const record = await noticeRecord();
        if (!record || record.sub !== push.to) return null;
        const plain = await self.pimbleNotice.open(push.sealed, record, push.to);
        const notice = JSON.parse(new TextDecoder().decode(plain));
        // The sender the service authenticated, as the server checks it.
        return notice && notice.from === push.from ? notice : null;
    } catch (e) {
        return null;
    }
}

self.addEventListener("push", (event) => {
    event.waitUntil((async () => {
        let push = null;
        try {
            push = event.data ? event.data.json() : null;
        } catch (e) {
            push = null;
        }
        if (!push || push.v !== 1) {
            await self.registration.showNotification("Pimble", { body: "You have a new notice.", icon: BASE + "pimble-icon.svg", data: {} });
            return;
        }
        const notice = await openedNotice(push);
        const title = notice ? titleOf(notice) : push.line || "Someone wrote to you on Pimble";
        const options = {
            body: notice ? notice.snippet || "" : "",
            icon: BASE + "pimble-icon.svg",
            // The page shows a notice it receives under the notice's id: one
            // notification for both.
            tag: notice ? notice.id : push.id,
            data: { notice: notice ? notice.id : null },
        };
        await self.registration.showNotification(title, options);
        if (push.seen) {
            try {
                await fetch(push.seen, { method: "POST", credentials: "omit" });
            } catch (e) {
                // Not told: it may be emailed as well.
            }
        }
    })());
});

self.addEventListener("notificationclick", (event) => {
    event.notification.close();
    const notice = event.notification.data && event.notification.data.notice;
    event.waitUntil((async () => {
        const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
        const open = windows.find((w) => (new URL(w.url).pathname + "/").startsWith(BASE));
        if (open) {
            open.postMessage({ t: "pimble-open-notice", notice: notice || null, inbox: !notice });
            await open.focus();
            return;
        }
        await self.clients.openWindow(notice ? BASE + "?notice=" + encodeURIComponent(notice) : BASE + "?inbox");
    })());
});
