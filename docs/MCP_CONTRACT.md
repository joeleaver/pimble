# MCP contract: an LLM reads, searches and writes your stores, as a collaborator

Status: written by the PM, 2026-10-01, from Joe's decisions of that day; approved by him
the same day ("Yes"), with node references and subtree listing added at his request.
Wave 1 in progress.

## The decisions (Joe, 2026-10-01)

1. **`pimble-mcp` starts its own server when none is running.** The desktop app joins it
   when it starts, exactly as it joins any running Pimble server today.
2. **An LLM edits like a collaborator.** Its edits merge like another device's. They are
   not marked, and the editor's undo does not take them back (it does not take back any
   other device's edits either). Marking them can come later.
3. **Write scope is everything the person could write themselves.** No allowlist, no
   per-store opt-in. What the server refuses the person (a read-only share, an ended
   share) it refuses the LLM, with the same sentence.

Agreed in the same conversation:

- **Transport is stdio.** The LLM client (Claude Desktop, Claude Code, ...) launches
  `pimble-mcp`, on Linux, macOS and Windows alike. Streamable HTTP served by the app's
  server can come later over the same tool code; it would need a token of its own, since
  the embedded server has none.
- **Cloud stores are reached through the local server, never directly.** A hosted store is
  end-to-end encrypted; the local server holds the account's keys (`keys.json`) and runs
  the vault link. To the MCP a hosted store added here is an open store like any other.
  A hosted MCP at pimble.app is not possible for vault stores without the server holding
  keys, and is not built.

## The process

`crates/pimble-mcp`, a binary `pimble-mcp` (`pimble-mcp.exe`), shipped in the same Linux
and Windows packages as `pimble` by `release.yml`. It is an MCP server on stdin/stdout
(the `rmcp` crate), one JSON message per line.

- **stdout carries the protocol and nothing else.** Logs go to stderr (`tracing` with a
  stderr writer). A stray `println!` anywhere in its dependency path is a bug.
- On Windows it is built so that a GUI client launching it shows no console window.
- Built with the same features as the app (`onnx-download`), so search is semantic
  whichever process owns the server.

### Which server it talks to

The same rule as the app, by the same code. `ensure_connected` and `reconnect` move out of
`crates/pimble-app/src/backend.rs` into `pimble_server::local` (pimble-server already
depends on pimble-client), and both binaries call them:

1. The address is `PIMBLE_APP_ADDR`, else `127.0.0.1:7462`. A `PIMBLE_SERVER` URL (with
   `PIMBLE_TOKEN`) names some other server instead, and then nothing is started.
2. A Pimble server already answering there is used (the probe: `listStores` within 2 s).
3. Otherwise `pimble-mcp` starts the embedded server itself and opens the stores in the
   app's saved list (`<config dir>/pimble/state.json`, `open_stores`), so the LLM sees
   what the person sees in the explorer.
4. When the server goes away (the app that owned it quit), `pimble-mcp` reconnects by the
   same rule: it joins another server or starts one, and opens the saved list again. A
   tool call made during the gap waits for the reconnect, up to 10 s, then fails with a
   sentence.

When `pimble-mcp` exits while owning the server, the server stops (flushing every dirty
document, as it does today) and a connected app reconnects by its own rule: it starts a
server and reopens its saved list. This handoff already works in the app (`reconnect`,
`BackendEvent::Connected` reopening the saved stores, the active document's reconcile); it
is part of the walk-through below because nothing has exercised it with two kinds of
client.

### The open-store list

`state.json`'s `open_stores` remains the one list of what is open on this computer.
`pimble-mcp` reads it when it starts a server, and adds a store's path to it when a tool
opens or adds one (`add_hosted_store`), so the explorer shows it next time the app
connects (the app's `StoresListed` already registers a store it did not know). The app
and `pimble-mcp` update the file by read-modify-write of that one key, as the app does
today. Two writers losing an update to each other is accepted (writes are rare and the
loss is a store missing from the list, never data).

## Principal and write scope

`pimble-mcp` connects as the local server's `Service` principal, like the app. It can do
anything the person can do in the app, and the server's own judgements (`StoreAccess`,
read-only roots, ended shares) refuse it what they refuse the person. Each refusal reaches
the LLM as the server's sentence, unchanged ("You can read this, not change it.").

Edits carry client id `mcp:<uuid>` (one per `pimble-mcp` process), so notifications and
logs say where an edit came from.

## Writing: the CRDT path, and nothing else

Every write is an edit of the node's existing document, made the way `set-node-text`
makes one (`crates/pimble-cli/src/main.rs`, `set_node_text`):

1. fetch the node's document (`getNode`, `NodeDoc::load`);
2. compute the edit locally as a delta on that document (a `NodeDoc` method);
3. send it with `applyEdit` (`IncrementalChanges`).

`updateNodeContent` is never used. Because the delta continues the document's own
history, whatever the person typed between steps 1 and 3 survives the merge: the edit
deletes only items it saw and inserts relative to items it saw.

Titles and tags go the same way (`NodeDoc::set_title` with `custom.explicit_title = true`,
`NodeDoc::set_tags`), as `applyEdit` of the `node` root, not through
`updateNodeMetadata`, which replaces the whole metadata from a copy that may be stale.
The server derives `NodeRenamed` from the merge, as for any device.

Structure goes through the RPCs that already make structural edits of node documents:
`createNode`, `moveNode` (a move out of a share is a delete there and a new node where it
lands, `docs/MOVE_CONTRACT.md`; the tool reports the new id and the shares left),
`deleteNode` (a tombstone; "Recently Deleted" puts it back) and `undeleteNode`.

### Markdown, in and out

The LLM reads and writes Markdown (CommonMark plus `~~strike~~`). The conversion lives in
`pimble-crdt` (`markdown.rs`: `blocks_to_markdown`, `markdown_to_blocks`), so the CLI and
the importer can use it too. rinch's editor core already has a Markdown serializer
(`rinch-editor-core/src/serialize/markdown.rs`, images and unsafe URLs included); wave 1
starts by deciding whether to build on it (going through the editor model) or to convert
straight between Markdown and `Block`.

- It covers exactly the collaboration scope, `pimble_crdt::Block`: paragraphs, headings
  1-6, code blocks, nested bullet and ordered lists, and the marks bold, italic,
  strikethrough, inline code and links. Underline, highlight, text colour and
  sub/superscript are written as the HTML tags `<u>`, `<mark>`, `<span style="color:..">`,
  `<sub>`, `<sup>`, and read back from them only.
- A link is a Markdown link; a Pimble link's href is its `pimble:` URL.
- Hard breaks (a trailing backslash or two spaces) and horizontal rules (`---`) are in
  rinch's collaboration scope at `63fee3a` and map both ways.
- **Block quotes and tables** join when rinch's PRs for them merge (joeleaver/rinch#1229,
  #1233) and `pimble_crdt::Block` gains `Blockquote` and `Table`: `>` quotes, and GFM
  pipe tables (a header row, no merged cells; a table read from a document with merged
  cells is written as HTML `<table>` with `colspan`/`rowspan`, and read back from it).
  Until then they are refused like the rest below.
- **Images** follow `docs/IMAGES_CONTRACT.md`: `![alt](pimble-blob:...)` both ways once
  blobs exist, and `attach_image` (a local file in, a blob and an inserted image out).
  Until then an image is refused.
- Whatever the editor cannot co-edit (raw HTML other than the tags above, footnotes, task
  lists) is **refused**, not degraded: the tool fails with a sentence naming the
  construct and its line ("Line 12 is a task list; Pimble documents cannot hold task
  lists yet."). Nothing is written.
- Reading a node whose content `NodeDoc::blocks` cannot project answers its plain text
  (`NodeDoc::text`) with a note saying formatting was left out.

### Edits that touch only what they name

A whole-document rewrite drops what Markdown cannot say, and fights with a person typing
in the same node. So there is none. The write tools name the blocks they change:

| Tool | Edit |
| --- | --- |
| `append` | blocks after the content (`NodeDoc::append_blocks`, exists) |
| `insert_after` | blocks after the block a quote names, or after a heading's section |
| `replace_section` | the blocks under a heading, up to the next heading of its level or higher |
| `replace_text` | an exact quote inside one block, replaced by new text; marks outside the quote untouched |
| `write_new` | the content of a node whose content was never written (a node `create_node` just made) |

A quote names a block by its text, the way a deep link's quote does
(`docs/LINKS_CONTRACT.md`, `quote`). It must match exactly one block; zero or several
matches fail with a sentence that says which, and how many.

New in `pimble-crdt`, both edits of the existing history like `replace_plain_text`:

- `NodeDoc::replace_blocks(range, blocks) -> delta` (the block-range edit `insert_after`
  and `replace_section` are made of; an empty range is an insert);
- `NodeDoc::replace_in_block(block, quote, text) -> delta`.

Both have the convergence tests `replace_plain_text` has: a concurrent edit by another
replica, inside and outside the range, merged both ways, with the same result.

## The tools

Read:

| Tool | What it does |
| --- | --- |
| `list_stores` | open stores: id, name, kind, sync state, whether this device may write |
| `list_children` | a node's children (title, id, type, child count), through mounts; with `depth` > 1, the subtree as an indented outline, each line carrying its id (a store row's root when given a store) |
| `find_node` | nodes whose title matches (exact first, then prefix, then contains), each with its id and path; for "put this under the MEDICAL node" |
| `get_node` | id, `pimble:` link, title, tags, path from the root, child count, and the content as Markdown |
| `search` | keyword or semantic (`search`, `semantic` flag), with snippets |
| `resolve_link` | where a `pimble:` URL points now (follows `became`) |
| `list_deleted` | a store's recently deleted nodes |
| `list_hosted_stores` | the signed-in account's hosted stores, and which are open here |

Write:

| Tool | What it does |
| --- | --- |
| `create_node` | a child of a node, with a title and optional Markdown content |
| `append`, `insert_after`, `replace_section`, `replace_text` | as above |
| `rename`, `set_tags` | the node's title and tags |
| `move_node` | within a store, or between stores (`transplantNode`) |
| `delete_node`, `undelete_node` | tombstone, put back |
| `add_hosted_store` | open a hosted store here as a replica (`cloudAddHostedStore`) and add it to the open-store list |

Not exposed, on purpose: hosting a store (`cloudHostStore`), sharing, relaying, linking to
a remote, closing or removing stores, signing in or out. Nothing is hosted unless the
person asked for it (CLAUDE.md), and an LLM asking is not the person asking. Signing in
stays in the app's Account menu; when no account is signed in, the cloud tools answer
"Sign in from Pimble's Account menu first."

### Naming a node

Every tool that takes a node takes one string, `node`, which is any of:

- a `pimble:` link (`pimble:<store>/<node>`, what the explorer's "Copy Link" puts on the
  clipboard), anchor ignored;
- a bare node id (a UUID), looked up in every open store;
- a path of titles from a store's name, `/`-separated (`Family Management/MEDICAL/Dr
  Smith`), matched case-insensitively, through mounts.

Ambiguity is an error, never a guess: a path or id that matches several nodes answers
each candidate's id and path, so the LLM (or the person) picks one. Every node a tool
returns carries its id, its `pimble:` link and its path, so anything listed can be named
in the next call. A node's id is shown in the app too: its row's "Copy Link" gives the
link, which is what a person pastes into the LLM's chat.

## Waves

1. **`pimble-crdt`:** `markdown.rs` (both ways, the refusals, round-trip tests over every
   `Block` and mark), `replace_blocks`, `replace_in_block`, their convergence tests.
2. **The shared connection:** `pimble_server::local::{ensure_connected, reconnect}`, the
   app moved onto it, the saved-list helpers moved beside it (`state.json` read and
   write), with no change in the app's behaviour.
3. **`pimble-mcp`:** the crate, the tools, stderr logging, the Windows build flag, the
   release packaging, a section in `docs/DEPLOY.md` with the client config for Claude
   Desktop and Claude Code on Linux and Windows.
4. **Walk-through** (PM, on a store copy with `PIMBLE_APP_ADDR=127.0.0.1:7473`):
   - with the app running, the LLM appends to and rewrites a section of the node open in
     the editor while the PM types in it; both edits survive, the editor shows the LLM's
     live;
   - with the app closed, `pimble-mcp` starts the server and opens the saved stores; the
     app started afterwards joins it; quitting the LLM client hands the server back to
     the app with nothing lost, and the other way round;
   - a hosted store added through the MCP appears in the explorer, encrypted, and an edit
     made there reaches the browser;
   - a read-only share refuses a write with its sentence;
   - a table in the LLM's Markdown is refused with its line, nothing written;
   - the same on Windows: the config, no console window, the server handoff.

## Later

- Streamable HTTP on the app's server, with a token of its own.
- Marking or attributing LLM edits; an undo for them.
- A write scope narrower than the person's (an allowlist, or read-only mode).
