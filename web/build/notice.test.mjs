// node web/build/notice.test.mjs
//
// notice.js (the service worker's opening of pushed notices) against a notice
// pimble_crypto sealed (`cargo test -p pimble-crypto print_a_notice_vector --
// --ignored --nocapture`), with the notice key imported as the browser's own
// server imports it (PKCS#8, non-extractable, X25519); and its parts against
// the published vectors.
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
require("./notice.js");
const { open, poly1305, hchacha20 } = globalThis.pimbleNotice;

const hex = (h) => Uint8Array.from(h.match(/../g).map((b) => parseInt(b, 16)));
const toHex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

// RFC 8439 section 2.5.2.
assert.equal(
    toHex(poly1305(hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"), new TextEncoder().encode("Cryptographic Forum Research Group"))),
    "a8061dc1305136c6c22b8baf0c0127a9",
);

// draft-irtf-cfrg-xchacha section 2.2.1.
assert.equal(
    toHex(hchacha20(hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"), hex("000000090000004a0000000031415927"))),
    "82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc",
);

// A notice pimble_crypto sealed to NoticeKey { secret: [7; 32] } for "will-account".
const VECTOR = {
    secret: "0707070707070707070707070707070707070707070707070707070707070707",
    public: "E75P6uryBMf9M1j8nAByGIHRdCeBKCJ-xnTzf3_pe20",
    sealed: "AY1HDqbAGElOabMjzxF9zVbV8srBodfD2t77H5FfWMNAbSN8ltPuA3lU5_01X60iUjzoo6lgtVxMxGcqELhOSdeaTDhcnR192aKoJiU8CZVZASJ-5bJIFVf28hUZRHG_8C3MvI48bWOFA5yMx93ThfmVcL_rfXg8lATjK7U",
};

const pkcs8 = new Uint8Array([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20, ...hex(VECTOR.secret)]);
const key = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "X25519" }, false, ["deriveBits"]);
const record = { sub: "will-account", key, public: VECTOR.public };

const plain = new TextDecoder().decode(await open(VECTOR.sealed, record, "will-account"));
assert.deepEqual(JSON.parse(plain), { id: "n1", snippet: "@Will can you check the oven" });

// Sealed for one account, it does not open as another's.
await assert.rejects(open(VECTOR.sealed, record, "someone-else"));
// Tampered with, it does not open.
const tampered = VECTOR.sealed.slice(0, -2) + (VECTOR.sealed.endsWith("A") ? "BB" : "AA");
await assert.rejects(open(tampered, record, "will-account"));
// Another key does not open it.
const other = new Uint8Array(pkcs8);
other[other.length - 1] ^= 1;
const otherKey = await crypto.subtle.importKey("pkcs8", other, { name: "X25519" }, false, ["deriveBits"]);
await assert.rejects(open(VECTOR.sealed, { ...record, key: otherKey }, "will-account"));

console.log("notice.js: 6 checks passed");
