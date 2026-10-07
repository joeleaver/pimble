# Images contract: pictures in a node's text, stored once beside the store

Status: written by the PM, 2026-10-01, from Joe's choice of that day (a blob store per
store, over images inline in the document or links only). Wave 1's server side is built
on branch `images` (2026-10-06, not merged): see "Built in wave 1 (server side)" at the
end. The app's loader and inserting are built on branch `pictures-ui` (2026-10-07, not
merged), for the desktop and for plain stores in the browser: see "Built in wave 1 (app
side)". Waves 2 to 5 are not: pictures do not replicate, and an encrypted store in the
browser takes and shows none.

## The decisions (Joe, 2026-10-01)

1. **A blob store per store**, over images inline in the document or links only.
2. **20 MiB per image.** A larger image the person pastes, drops or picks is **scaled down
   to fit, with a notification**, never refused (see "Too large").
3. **Eager fetch:** every replica holds every picture its documents name, for offline use.
4. **No deleting:** a blob is never deleted while its store exists, because undo, history
   and "Put Back" can bring a reference back.

## The shape

An image is rinch's `image` node (an inline atom, already in the collaboration scope at
rinch `301cbbb`) in a node's text, with `src` a Pimble blob URL:

```
pimble-blob:<store uuid>/<blob id>
```

The bytes live once, beside the store, as an **immutable blob**. The reference in the text
is co-edited like every other character: inserted, moved, deleted, undone and merged by the
one write path (`applyEdit`). The blob itself is never edited, so there is nothing to merge
and nothing to keep in step; it only has to be present wherever a document that names it
is. That is the whole design, and it keeps the rule that everything a person edits is
co-editable.

- **A blob id is random** (128 bits, base32), not a hash of the bytes. In an encrypted
  store the hosted server sees ids; a hash of the plaintext would let it confirm that a
  store holds a known picture. Integrity comes from the blob's own header (below) and, in
  a vault, from the AEAD. Pasting the same picture twice stores it twice; that is accepted.
- **Images only:** PNG, JPEG, GIF, WebP, AVIF. Never SVG (it can carry script; rinch's own
  HTML path already refuses `data:image/svg+xml`). Limit 20 MiB per image (decision 2).
- The store uuid in the URL is the canonical store, as in `pimble:` links, so an image
  shown through a mount or copied into another store's text still names where it lives.
  Copying a node between stores (`transplantNode`) copies the blobs its text names into the
  destination and rewrites the URLs in the transplanted copy's text, in the same edit.

## On disk (a plain store, a replica)

`<store>/blobs/<blob id>`: a header (magic `PIMG`, version, MIME type, byte length,
SHA-256 of the image) followed by the image bytes. Written to a temp name and renamed, so a
blob is either whole or absent. The header's hash is checked on every read; a mismatch is
an absent blob plus a log line.

What a node references is derived from its content, like its links:
`NodeDoc::blob_refs() -> Vec<BlobRef>` (and `blob_refs_of(bytes)`), reading every `image`
atom in the `content` root.

## RPCs

| RPC | What |
| --- | --- |
| `putBlob { store_id, node_id, mime, bytes }` → `{ url }` | store a new blob for an image about to be inserted in `node_id` (authorized as a write of that node; `node_id` is what scoped grants judge) |
| `getBlob { store_id, blob_id }` → `{ mime, bytes }` | read one (authorized as a read of any node that may reach it; below) |
| `haveBlobs { store_id, ids }` → `{ missing }` | which of these this server lacks |

`bytes` travel base64 in JSON-RPC, under the 16 MiB message limit after encoding for
anything up to the 20 MiB image cap by chunking: `putBlob`/`getBlob` take and return
`offset`/`total` and a blob is assembled server-side before it is renamed into place.

## Who may read a blob

A blob is readable by whoever may read a node whose text names it. On a plain store the
server knows this (it reads the documents: `Reach` in `handler.rs` judges a blob by the
nodes that reference it, through an index `blob id -> referencing nodes` kept beside the
search index and rebuilt with it). A blob still named only by a tombstone stays readable
to whoever may read that tombstone, so "Put Back" brings its pictures back.

## Replication

- **Sync link (plain replica):** after each reconcile and each live update the link
  collects the blob refs of the documents it applied, asks `haveBlobs` of whichever side
  lacks them and copies them across with `getBlob`/`putBlob`. A missing blob never blocks
  a document: the text arrives first, the picture when its bytes do.
- **Eager:** a replica fetches every blob its documents name, so it works offline
  (decision 3).
- **Encrypted stores (vault link, relay, shares):** a blob is a vault document of its own,
  `blob:<blob id>`, with one entry (or several for an image over the 3 MiB catch-up size,
  each a chunk), encrypted in the `PB` layout under **its own data key** with associated
  data `"{store_id}/blob:{blob id}"`. Its data key is wrapped under every scope key that
  may read it, exactly as a document's is (`WrappedDek`, `vaultSetDocKeys`): the owner's
  share upkeep (`share.rs`) wraps a blob's key for a share when a node in that share names
  it, and adds the blob to the share's published scope set, the same as it does for a
  document entering a share; a member who inserts an image in a share uploads it with the
  share key's wrap on its first `vaultAppend`, and the server extends the scope set as it
  does for a new document naming a parent the member may write. The hosted server sees an
  id and ciphertext, nothing else.
- **The browser** fetches and decrypts a blob through the vault client when an image needs
  it and hands the editor an object URL (`blob:`); keys stay in memory as today.

Nothing about images changes where data goes: a blob is uploaded only to where the
store's documents already go (its hosted twin, its relay, a linked server). An unhosted
store's images stay on the machine.

## Showing and inserting images

- **Desktop:** an `ImageLoader` the app installs resolves `pimble-blob:` URLs by `getBlob`
  on the local server, and passes everything else to rinch's default loader.
  (`http:`/`https:` images keep loading as rinch loads them.) A missing blob shows a
  placeholder with "This picture has not arrived yet." and loads when it does.
- **Inserting:** paste an image, drop an image file, or Edit > "Insert Image..." (a file
  dialog). Each one calls `putBlob` and then `EditorHandle::insert_image(url, alt)`; the
  insert is an ordinary local edit and travels like typing.
- **Web:** the same three ways, `putBlob`'s vault equivalent through the vault client, and
  an object URL for display.
- **Too large** (decision 2): before `putBlob`, an image over 20 MiB is scaled down, in
  the client that is inserting it (desktop, web, MCP alike: one function,
  `pimble_core::image_fit` or a small crate of its own, on the `image` crate, native and
  wasm). It keeps its aspect ratio and is scaled by `sqrt(limit / size)`, then by 0.9 per
  step until the encoding fits. It is re-encoded as PNG when it has transparency, else as
  JPEG at quality 90 (WebP and AVIF become one of those two; nothing else in the stack can
  write them). EXIF orientation is applied first and the rest of the metadata is dropped.
  The insert then goes ahead, and the person is told in one sentence: "This picture was
  34.2 MiB, so it was scaled to 4032 × 3024 (18.6 MiB) to fit Pimble's 20 MiB limit." (a
  notification on the desktop and in the browser; the MCP tool's answer says the same).
  An animated GIF over the limit cannot be scaled frame by frame here: its first frame is
  kept as a still, and the sentence says so. The server enforces the limit regardless and
  refuses an oversize `putBlob` with "Pictures can be at most 20 MiB."
- **Search** indexes an image's `alt` text as part of the block it sits in.
- **Markdown** (docs/MCP_CONTRACT.md): `![alt](pimble-blob:...)` both ways; the MCP's
  `attach_image` tool (a local file path in, a blob and an inserted image out) comes in
  the MCP's own waves.

## rinch: what must go upstream (pull requests on joeleaver/rinch)

1. **An app-installed image loader.** Today `App` hard-wires `NetworkImageLoader` (with
   `image-network`) or `FileImageLoader`. Add `App::image_loader(Arc<dyn ImageLoader>)`
   (or a scheme-keyed resolver that falls back to the default), on the desktop.
2. **The same on the web:** a resolver `src -> Option<String>` that `rinch-web` consults
   before handing an `<img>` its `src`, so `pimble-blob:` can become an object URL.
3. **Image paste and drop in the editor:** an `EditorHandle` hook,
   `on_image_input(|ImageInput { bytes, mime, name }| -> Option<(src, alt)>)`, called when
   the clipboard holds an image or an image file is dropped, with the insert made by the
   handle when the app answers. Without the hook, paste of an image does nothing (never a
   `data:` URL in a collaborating document: that is the inline option Joe did not choose).
4. **Collab check:** confirm an `image` atom's `src`, `alt`, `title` attrs round-trip and
   merge (they are in scope at `301cbbb`; a test that changes `alt` on two peers
   concurrently).

## Pimble side

- `pimble_crdt::Block`: `Run` gains an inline image (`Inline::Image { src, alt, title }`)
  alongside text, plus `HardBreak`, and `Block::HorizontalRule`, so `blocks()` and
  `from_blocks` cover what rinch's collaboration scope now covers (done; block quotes
  and tables joined with v0.6.0).
- `NodeDoc::blob_refs`, the blob store in `pimble-store`, the three RPCs, `Reach` for blobs,
  the sync link and vault link carrying them, share upkeep wrapping their keys.
- The app's loader, the paste/drop/menu insert, the placeholder.
- The web's vault-client fetch and object URLs.
- The Scrivener importer brings a document's pictures in as blobs.

## Waves

1. rinch PRs 1, 3 and 4; the `Block` additions; `blob_refs`; the blob store and the three
   RPCs; the desktop loader and inserting. Verified on one machine: paste a screenshot,
   reopen the store, it is there.
2. Plain replicas: the sync link carries blobs; `Reach` for blobs; transplant copies them.
   Verified with two servers and a cut link.
3. Encrypted: blobs as vault documents, data keys and wraps, share upkeep, members
   inserting images in a share, the relay. Verified on the local stack: only ciphertext on
   the hosted disk, a member sees the owner's picture with the owner offline.
4. Web: rinch PR 2, the vault client's fetch, paste and drop in the browser.
5. Importer pictures; the MCP's `attach_image`.

## Built in wave 1 (server side)

Storage and RPCs for a plain store or replica; nothing in the app, the sync link, vault
stores or the web yet.

- **`pimble-core`** (`blob.rs`): `BlobUrl` (`pimble-blob:<store uuid>/<blob id>`, one
  parser, one printer), `BlobId` (128 random bits as 26 characters of lowercase base32,
  parsed strictly: it is a file name), `ImageMime` (the five accepted types, `parse` and a
  magic-number `sniff`), `MAX_IMAGE_BYTES`, and the two refusal sentences.
- **`pimble-crdt`**: `NodeDoc::blob_refs` / `blob_refs_of` (`BlobRef { url, alt, path }`
  for every `image` whose `src` is a blob URL, in document order, at any depth), read
  from the projected model as links are. `Block` gained `HorizontalRule`, and a
  paragraph's or heading's content is `Vec<Inline>`: `Text(Run)`, `Image(Image { src,
  alt, title, marks })`, `HardBreak`.
- **`pimble-store`** (`blobs.rs`): `BlobStore`, reached as `LocalStore::blobs()` and
  `StoreManager::blobs(store)`. `<store>/blobs/<blob id>` is `PIMG`, a version byte (1),
  the MIME type (a length byte and the bytes), the image's length (`u64` LE), its
  SHA-256, then the image. Written as `.<blob id>.part` and renamed; every read checks
  length and hash. `put`, `put_as` (a given id: the same bytes are a no-op, other bytes
  an error), `get`, `has`, `missing`, `upload_chunk`, and `sweep_partial` (run when the
  store opens; a new upload also clears parts an hour old).
- **RPCs**: as the table above, with chunking spelled out. `putBlob { store_id, node_id,
  mime, bytes, offset?, total?, blob_id? }` answers `{ url, received, complete }`: a
  picture that fits one message is sent whole; otherwise the first request carries
  `total`, the answer's `url` names the id, and each later chunk names that `blob_id` and
  the `offset` it continues from, in order. `getBlob { store_id, blob_id, offset? }`
  answers `{ mime, bytes, offset, total }`, at most one chunk at a time. A chunk is 3 MiB
  (`BLOB_CHUNK_BYTES`; 4 MiB as base64): the message limit in force is jsonrpsee's
  default of **10 MiB**, not 16, since neither the server nor the client sets one.
  `PimbleClient::put_blob` / `get_blob` / `have_blobs` hide the chunks.
- **Who may**: `putBlob` is judged as `applyEdit` on `node_id` is (role, a member's
  scope, a read-only replica); for a mount node the picture goes to the mount's source
  store, judged there, and the URL names that store. `getBlob` / `haveBlobs` are for the
  operator and whole-store readers; **a share's member is refused** with "Pictures in
  shares are not available yet." until wave 2 judges a blob by the nodes that name it
  (`TODO(images wave 2)` in `handler.rs`). A vault store answers `-32005`.
- **CLI**: `put-blob <store> <node> <file>` (prints the URL; the type comes from the
  file's first bytes) and `get-blob <url> <out file>`.

## Built in wave 1 (app side)

Built on branch `pictures-ui` (2026-10-07, not merged), on the storage half above and
rinch's `pimble/images` branch. One module, `crates/pimble-app/src/pictures.rs`, serves the
desktop and the browser; only the file picker differs between them.

- **Protocol** (`protocol.rs`, `commands.rs`): `BackendCommand::PutBlob { store_id, node_id,
  bytes, request_id }` answers `BlobStored { request_id, result }` with the `BlobUrl` and
  the notice sentence, or a refusal sentence; `GetBlob { url }` answers `BlobLoaded`. Both
  run as tasks beside the command loop, so typing does not wait behind a picture, and on
  the desktop `pimble_image::fit` runs on a blocking thread. In the browser the page waits
  while an oversize picture is scaled (one thread).
- **Inserting**: a pasted picture and a dropped image file reach the app through
  `EditorHandle::on_image_input`, registered on each pane's editor; Edit > "Insert
  Image..." is rinch's file dialog on the desktop and a hidden `<input type="file">` in
  the browser. All three call `add_picture`, which keeps the anchored place, sends
  `PutBlob`, and inserts with `insert_image_at` when the URL comes back (at the caret when
  a peer's change re-projected the document meanwhile and the anchor is gone). `alt` is the
  file's name without its extension, empty for a pasted bitmap. A note this device may
  only read, or a pane with no note, answers with a sentence and sends nothing. The notice
  and every refusal go to the status bar's notice line.
- **Showing**: one loader for the `pimble-blob:` scheme (`rinch::image::register_image_scheme`,
  which rinch-web honours too by making the object URL itself, so there is no
  browser-only resolver). It never blocks: a picture it does not hold is asked for with
  `GetBlob` once and answered "not yet", and `rinch::image::reload_image` is called when
  `BlobLoaded` brings the bytes. Fetched bytes sit in a cache keyed by `BlobUrl`, 64 MiB,
  oldest out first. A picture that is not here is asked for again when a document opens
  and when the connection comes back. Pictures are never wider than their pane.
- **Pasted HTML** (`PicturesPlugin::handle_paste`): every `<img>` whose `src` is not a
  `pimble-blob:` URL of a store open here is left out, with a sentence; a top-level block
  that held only such pictures goes with them. No `data:` or remote address enters a
  document. Remote pictures are not fetched into blobs.
- **Encrypted stores**: in the browser a vault store (open or only known from the
  account's list) answers `PutBlob` with "Pictures in encrypted stores are not available
  in the browser yet." and writes nothing, and its pictures are not fetched. On the
  desktop a hosted store's replica is a plain local store, so pictures are added and shown
  there, but **they do not travel**: neither the sync link nor the vault link carries
  blobs yet (waves 2 and 3), so another device sees the image's place and no picture.
- **CLI**: `blob-refs <store> <node>` prints the pictures a node's text names.
- **Verified** (2026-10-07): on the desktop, in an isolated app driven over the debug
  port, "Insert Image..." with a PNG (the blob read back byte for byte with `get-blob`),
  the paste and drop hooks through `PIMBLE_DEBUG_PICTURE_AS` (a real clipboard paste and
  a real OS drop were not exercised), the same note in two panes, an edit from the CLI
  beside it, a restart, a 24.3 MiB PNG scaled with its notice, an SVG and a text file
  refused, and a read-only store refused. In Chromium on the local stack, in a plain
  hosted store: a synthetic paste event and the file input (handed its file by script, the
  browser's own chooser not opened) stored, inserted and showed pictures, and they were
  still there after a reload; in an encrypted store the refusal sentence and nothing
  inserted. Firefox was not run.
- **Not built, against the text above**: the placeholder that says "This picture has not
  arrived yet." (a missing picture shows whatever rinch shows for an image that failed to
  load); a share's member on a plain hosted store is still refused `getBlob` by the
  server, so sees no pictures; nothing tells the app that a blob arrived, so a missing
  picture appears when its note is next opened, not the moment it lands. A picture stored
  for a note that was closed before the upload finished stays in the blob store,
  unreferenced. `PIMBLE_DEBUG_PICTURE` (and `PIMBLE_DEBUG_PICTURE_AS=paste|drop`) names a
  file that "Insert Image..." takes instead of opening the dialog, for driving the
  desktop app over rinch's debug port; it is read only when set.

## Later

- Sweeping unreferenced blobs.
- Attachments that are not images (PDFs, any file) on the same blob store.
- Resizing and captions (rinch image attrs).
- Thumbnails for large images.
