// Pimble's service worker (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 4",
// the follow-ups): keeps this build's own files so the page opens with no
// network and runs on the browser's own server from OPFS. Registered by
// hub.js on the default path (the browser's own server); the old path
// (`?server=old`, until wave 5) unregisters it and drops its caches.
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
