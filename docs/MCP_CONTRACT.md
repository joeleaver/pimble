# MCP contract: an LLM reads, searches and writes your stores, as a collaborator

Status: written by the PM, 2026-10-01, from Joe's decisions of that day; approved by him
the same day ("Yes"), with node references and subtree listing added at his request.
Waves 1-3 built 2026-10-01 on branch `mcp`; the walk-through passed on Linux (every tool
on a store copy, the app and `pimble-mcp` editing one paragraph at once, the server handed
over in both directions), `scripts/mcp/`. Not yet run on Windows, not merged.

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
2. A Pimble server already answering there is used (the probe: `listStores` within 2 s),
   when it is this build's version or newer (`serverInfo`, from 0.12.2). One that is older
   and says `hands_over` (the app's own loopback server, never a hosted one) is asked to
   (`handOver`: it answers, writes every document, stops its links and frees the port), and
   the asker starts a current server in its place; the process whose server handed over
   reconnects by joining it and starts none of its own on that reconnect. One that is older
   and cannot be asked (any server from before 0.12.2) is refused with a sentence that names
   the process holding the port ("An older Pimble server (from before 0.12.2) is running on
   127.0.0.1:7462. Quit pimble-mcp (process 2065674), then start Pimble again."). Added
   2026-10-21 after a `pimble-mcp` left running for a day from an older dev build served the
   0.12.1 app, whose Discuss button then answered "method not found". Only a
   `serverInfo` answered with "method not found" reads as a server from before 0.12.2; a
   connection that drops during the probe, or during `handOver` (another process asked
   first), is a server stopping, and the caller tries again as it does for a free port
   (2026-11-18: two Braynard sessions were told to quit a "server from before 0.12.2" that
   was a 0.22.9 server handing over to 0.23.0, and the calls cleared a minute later).
3. Otherwise `pimble-mcp` starts the embedded server itself and opens the stores in the
   app's saved list (`<config dir>/pimble/state.json`, `open_stores`), so the LLM sees
   what the person sees in the explorer.
4. Every connection takes messages up to `pimble_rpc::MAX_MESSAGE_BYTES` (512 MiB) both
   ways, the server's `Server::builder()` and the native `WsClientBuilder` alike (the
   browser's `WebSocket` has no limit to set); before 2026-11-18 jsonrpsee's 10 MB default
   applied, and one canvas past it made its store unloadable for every new connection
   ("Response is too big", docs/DRAWING_CONTRACT.md "Large canvases"). The hosted path
   adds no limit of its own: jkbase's proxy splices the upgraded WebSocket's bytes
   (`jkbase-wsproxy`, a 600 s idle timeout only). The relay keeps its own 16 MB message
   limit (`RelayLimits`), which `syncNodes` never meets there (relayed stores are vaults,
   4 MiB per blob).
5. The app hosts whenever it can (2026-11-19). `serverInfo` says which program runs a
   server (`host`: `"app"`, `"mcp"`, empty before 0.28.2), and the app asks a server of its
   own version that it does not run to hand over, then starts its own; `pimble-mcp` joins it
   on its next reconnect. A server lives as long as the program running it, and an MCP lives
   only as long as the chat that started it. (0.28.1 was installed under a running 0.28.0
   app; a 0.28.1 `pimble-mcp` took the port by the newer-version rule and carried every
   share for the rest of the day.) A newer server is still never asked to make way for an
   older app: restarting the app brings them level.
6. A server that `pimble-mcp` runs writes its log to `<data dir>/pimble/logs/server.log`
   too (`server.previous.log` the one before), from the moment it starts; its standard
   error goes to the chat, where nobody reads it. The app logs where it always has.
7. When the server goes away (the app that owned it quit), `pimble-mcp` reconnects by the
   same rule: it joins another server or starts one, and opens the saved list again. A
   tool call made during the gap waits for the reconnect, up to 10 s, then fails with a
   sentence.

When `pimble-mcp` exits while owning the server, the server stops (flushing every dirty
document, as it does today) and a connected app reconnects by its own rule: it starts a
server and reopens its saved list. This handoff already works in the app (`reconnect`,
`BackendEvent::Connected` reopening the saved stores, the active document's reconcile); it
is part of the walk-through below because nothing has exercised it with two kinds of
client.

Hosted stores are not an agent's: `list_hosted_stores` and `add_hosted_store` were removed
on 2026-10-21. They call `Service`-only RPCs (an agent never gets the store lifecycle or
cloud ones, docs/AGENTS_CONTRACT.md), so for an agent they only ever answered "cloudStatus
is only available to this server's own operator". A person adds a hosted store from
Account > Add Hosted Store..., and the agent sees it once its grants reach it.

### The open-store list

`state.json`'s `open_stores` remains the one list of what is open on this computer.
`pimble-mcp` reads it when it starts a server, and adds a store's path to it when a tool
opens one, so the explorer shows it next time the app
connects (the app's `StoresListed` already registers a store it did not know). The app
and `pimble-mcp` update the file by read-modify-write of that one key, as the app does
today. Two writers losing an update to each other is accepted (writes are rare and the
loss is a store missing from the list, never data).

## Principal and write scope

`pimble-mcp` connects to the local server with its agent key (`PIMBLE_AGENT_KEY`,
docs/AGENTS_CONTRACT.md), as the grants that agent was given. Within them it can do what the
person can do in the app, and the server's own judgements (`StoreAccess`,
read-only roots, ended shares) refuse it what they refuse the person. Each refusal reaches
the LLM as the server's sentence, unchanged ("You can read this, not change it.").

Edits carry client id `agent:<id>` (the agent the key names, docs/AGENTS_CONTRACT.md), so
notifications and logs say where an edit came from.

## Writing: the CRDT path, and nothing else

Every write is an edit of the node's existing document through the one write,
`pimble_api::Pimble::edit` (docs/API_CONTRACT.md decision 4; the CLI's `set-node-text`
uses the same call):

1. the node's document is held (read once, kept current by the store's subscription)
   and brought up to date with the server's before the edit (`syncNodes` of that one
   document), so a quote is judged against what the server holds now;
2. a write this agent may not make is refused before anything is sent: read-only, and
   since 2026-11-02 comment-only too (a commenter writes discussions, not notes; the
   server judges again);
3. the edit is computed locally as a delta on that document (a `NodeDoc` method), and
   sent with `applyEdit` only when merging it into the document as it was changes
   anything ("That would change nothing, so nothing was written." otherwise).

(`updateNodeContent` no longer exists.) Because the delta continues the document's own
history, whatever the person typed between steps 1 and 3 survives the merge: the edit
deletes only items it saw and inserts relative to items it saw.

Titles and tags go the same way (`NodeDoc::set_title` with `custom.explicit_title = true`,
`NodeDoc::set_tags`), as `applyEdit` of the `node` root (`updateNodeMetadata`, which
replaced the whole metadata from a copy that may be stale, was removed on 2026-11-02).
The server derives `NodeRenamed` from the merge, as for any device.

Structure goes through the RPCs that already make structural edits of node documents:
`createNode`, `moveNode` (a move out of a share is a delete there and a new node where it
lands, `docs/MOVE_CONTRACT.md`; the tool reports the new id and the shares left),
`deleteNode` (a tombstone; "Recently Deleted" puts it back) and `undeleteNode`.

### Markdown, in and out

The LLM reads and writes Markdown (CommonMark plus `~~strike~~`). **Decided in wave 1:
the conversion is rinch's own** (`rinch_editor_core::serialize::markdown`), between
Markdown and the editor model, never through `pimble_crdt::Block`: the model that comes out
of a document is the one an edit goes back in through, so a block nobody names keeps
everything the collaboration scope knows about it. `pimble_crdt::markdown` adds the strict
pre-pass (`check`: the refusals below, with line numbers, since rinch's parser drops what
it does not know silently) and the block edits; `NodeDoc::markdown`, `write_markdown`,
`append_markdown`, `insert_markdown_after`, `replace_section_markdown` and `replace_text`
are the API, each answering the delta it applied or `CrdtError::Refused(sentence)`.
rinch's Markdown (joeleaver/rinch#1242, on `main`) writes and reads the five marks below
as HTML tags, builds and writes tables, moves whitespace at a mark's edge outside its
delimiters, and has a strict parse (`doc_from_markdown_strict`) that names what it would
drop; `check` is Pimble's own refusals (images, task lists, inline code inside a mark)
and then that parse.

- It covers the collaboration scope: paragraphs, headings 1-6, code blocks, nested
  bullet and ordered lists, block quotes, tables, and the marks bold, italic,
  strikethrough, inline code and links. Underline, highlight, text colour and
  sub/superscript are written as the HTML tags `<u>`, `<mark>`, `<span style="color:..">`,
  `<sub>`, `<sup>`, and read back from them only.
- A link is a Markdown link; a Pimble link's href is its `pimble:` URL.
- **A mention** of a person (docs/NOTIFICATIONS_CONTRACT.md section 1, 2026-11-23) is a link
  to their account: `[@Will](pimble-person:<sub>?m=<id>)`. `?m=` is the mention's own id;
  a mention written without one is given one where it enters the document
  (`pimble_crdt::mentions::with_ids`), and get_node shows it, so a rewrite that keeps the
  href mentions nobody again. A `pimble-person:` href that names no account is refused.
  `list_people` says whom a note can mention.
- Hard breaks (a trailing backslash, or `<br>` where that does not read back) and
  horizontal rules (`---`) map both ways.
- **Block quotes and tables** (in since rinch `51cb7c7`, joeleaver/rinch#1229 and #1233):
  `>` quotes, and GFM pipe tables for a table with a header row, no merged cells and one
  paragraph per cell; any other table is written as HTML `<table>` (`colspan`/`rowspan`,
  blocks in cells) and read back from it. `pimble_crdt::Block` has `Blockquote` and
  `Table` for the importer.
- **Pictures are canvases** (docs/CANVAS_CONTRACT.md). Going out, a picture in text is
  written with its canvas's id after the URL: `![alt](pimble-blob:<store>/<hash>#canvas=<id>)`,
  or `pimble-board:in-text#canvas=<id>` for a canvas that began empty; a picture from before
  canvases has no fragment, and an embedded canvas node is `pimble-board:<store>/<node>`.
  **Going in (2026-10-29)**, those same addresses are accepted, so an assistant that rewrites
  a note keeps its pictures: the fragment goes back into the atom's `data-board`
  (`markdown::with_canvas_attrs`), and every block edit is judged by
  `markdown::check_canvases` against the canvases the note's `data` holds
  (`NodeDoc::held_canvases`): a picture may name only a canvas the note holds, and only
  once; pictures that were already there are judged as they were. Any other image
  (`https:`, a file, `data:`) is refused with its line; a new picture goes in with
  `insert_canvas`.
- Whatever the editor cannot co-edit (raw HTML other than the tags above, footnotes, task
  lists) is **refused**, not degraded: the tool fails with a sentence naming the
  construct and its line ("Line 12 is a task list; Pimble documents cannot hold task
  lists yet."). Nothing is written.
- Reading a node whose content `NodeDoc::blocks` cannot project answers its plain text
  (`NodeDoc::text`) with a note saying formatting was left out.

### Edits that touch only what they name

A wholesale rewrite (a fresh document, or one edit spanning every block) drops what
Markdown cannot say, and fights with a person typing in the same node. So there is none.
The write tools name the blocks they change:

| Tool | Edit |
| --- | --- |
| `append` | blocks after the content (`NodeDoc::append_blocks`, exists) |
| `insert_after` | blocks after the block a quote or `block_index` names, or after a heading's section |
| `replace_section` | the blocks under a heading, up to the next heading of its level or higher |
| `replace_text` | an exact quote inside one block (with `occurrence` when it appears several times), replaced by new text; marks outside the quote untouched |
| `replace_block` | the one top-level block a quote or `block_index` names, replaced by Markdown or removed when it is empty (a heading removed or re-levelled) |
| `remove_section` | a heading and everything under it, up to the next heading of its level or higher |
| `replace_content` | the whole content, written as the smallest set of block edits: blocks whose Markdown is the same before and after stay the same nodes, and each changed stretch is recorded as its own step (last first), so an unchanged block between two changed ones is never touched and anyone typing in it keeps their words. The node keeps its id and link |

`replace_block`, `remove_section` and `replace_content` were added on Joe's bug report of
2026-10-01: restructuring a note meant deleting and recreating it, which changed its
link. The same report found that `pimble-mcp` refused a replacement that only deletes as
"changing nothing" (it compared state vectors, which a deletion does not move; whether an
edit changes anything is now what merging it into the document as it was says,
`NodeUpdateEffect::changed`), and that bold, italic, strikethrough or a link around inline
code was dropped without a word: rinch's `code` mark excludes them, so `check` refuses
inline code inside any of them with a sentence that says how to write it instead.

A heading (for `insert_after` with `after_section`, `replace_section`, `remove_section`,
`get_node`'s `section` and `split_note`) is found one way, `markdown::find_heading`
(2026-11-18): some of its text, where a heading whose text is the name exactly
(`markdown::loose`: any case, punctuation and curly quotes ignored) wins over headings that
only contain it; or a heading path, the headings from the outermost in joined by `" > "` (as
search hits and outlines write them) or `"/"`, each one inside the section of the one
before. Several matches are refused with each one's heading path, so the next call can
name one.

### Naming what an edit touches (2026-11-23)

An agent once passed `block: "13"` with empty Markdown meaning block 13; the tool read it
as a quote, removed the paragraph holding the characters "13", and answered "Removed the
block". So a block, a heading and a place in the text are named in ways that cannot be
mistaken for each other, and every tool's description opens with that rule:

- **`quote`** (`replace_block`, `insert_after`, `replace_text`) is text, matched as text,
  the way a deep link's quote is (`docs/LINKS_CONTRACT.md`, `quote`): "13" is the
  characters 1 and 3. A list item's text names its whole list.
- **`block_index`** (`replace_block`, `insert_after`, and for a heading `replace_section`,
  `remove_section`, `insert_after` with `after_section`) is the block's number as
  `get_node`'s `outline` and `blocks` print it: top-level blocks from 0, none in an empty
  note. A number past the last is refused with the range there is; a section named by a
  block that is not a heading is refused with what that block is.
- **`heading`** (`replace_section`, `remove_section`) is some of a heading's text or a
  heading path, found as above.
- Exactly one of `quote`/`heading` and `block_index`; both or neither is refused.
- **`occurrence`** (1, 2, ... or `"last"`) picks among the places a quote appears: for a
  block, the blocks it appears in; for `replace_text`, every place in the text. Without
  it a quote must appear once; several are refused with every place listed, numbered as
  `occurrence` counts, each with its block number and the 30 characters either side
  (`markdown::quotes::pick`). It does not go with `block_index`.

The parameters were renamed (`block` and `after` became `quote`), and the edit tools'
parameters deny unknown fields, so a call with an old name is refused by name rather than
read as something else. There is no compatibility alias: the MCP has no compatibility
policy of its own, and `docs/API_CONTRACT.md` keeps no shims. A client that reads tool
schemas (every MCP client does at connect) sees the new names.

**Safe deletes.** Removing a block by a quote (`replace_block` with empty Markdown) is
refused when the quote is shorter than 12 characters (`markdown::MIN_DELETE_QUOTE`) or
reads like a number (digits and nothing but `#`, `.`, `-`, `,` and spaces). The sentence
says what it would have removed ("It would have removed block 2 (...); to remove that
block give `block_index: 2`"), every block when the quote appears in several, and when
the quote is a number of a block that exists, "If you meant block 13, give
`block_index: 13`". Replacing by a short quote stays allowed: what was there comes back in
the answer.

### What an edit says it did

Every edit that removes or replaces answers what was there (`markdown::Echo`): its block
numbers before the edit, its text run together and cut at 120 characters with "…"
(`EXCERPT`), and the text either side of it (60 characters, `NEIGHBOR`: the neighboring
blocks, or for `replace_text` the rest of the paragraph), or that it was the start or end
of the note. `insert_after` says between which blocks the new ones went.
`replace_content` lists each changed stretch with its old text (the first 20, then how
many more). The echo puts the old text in the agent's own transcript, which is also the
one recovery there is (below).

### Dry runs

Every destructive edit (`insert_after`, `replace_section`, `replace_text`,
`replace_block`, `remove_section`, `replace_content`) takes `dry_run: true`: the same
judgement, the same refusals and the same echo, starting "Dry run, nothing written", and
nothing sent (`NodeDoc::preview_markdown`). It goes through `Pimble::edit` with an edit
that changes nothing, so the document is brought up to date and the write's access is
judged as for the real edit, and an edit that would change nothing is refused as the real
one is.

### Recovery: an open question for Joe

There is no undo of an agent's edit, and nothing is built for it. What the documents can
honestly offer was looked at (2026-11-23):

- A node document is created with yrs's garbage collection on (`skip_gc` false), so the
  text of a removed block is gone from the document once the deletion is merged; there is
  no history to read it back from.
- **(a) Snapshots.** `skip_gc` on every replica, and a yrs `Snapshot` per agent edit to
  read the state before it. The documents grow forever; removed text stays in every
  shared and encrypted document, readable by every member of a share after its author
  removed it; and one replica with garbage collection on (an older version, the web)
  breaks it.
- **(b) An undo manager in the MCP.** A yrs `UndoManager` over the MCP's own edits keeps
  the items it tracks from collection and undoes by an ordinary new edit, which fits
  "everything is co-editable". But it lives in the MCP process's memory: a restart, a
  reconnect that reloads the held document, or a second MCP process loses it.
- **(c) What is given now.** The echo returns the old text (up to 120 characters a stretch)
  to the agent, which can write it back with `insert_after`; a deleted node comes back with
  `undelete_node`.

Which of these, if any, to build is Joe's call.

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
| `get_node` | id, `pimble:` link, title, tags, path from the root, child count, and the content as Markdown; or only part of it: `outline: true` (the headings, indented by level, each with its block number and its section's size in characters), `section` (one heading and its section, named as the section edits name one, heading paths included), `blocks` ("12" or "12-20", top-level blocks as the outline numbers them). A whole text note over 15,000 characters starts with one line giving its size and pointing at `outline` and `section`; all of it still follows |
| `search` | keyword or semantic (`search`, `semantic` flag), with snippets; each hit says where in the note it is (`in: Design > The county's edges`), a heading path `get_node`'s `section` takes as it is. Without `store`, every open store is searched and the hits of the store this session last worked in come first (below) |
| `resolve_link` | where a `pimble:` URL points now (follows `became`) |
| `list_deleted` | a store's recently deleted nodes |
| `list_people` | who can open a node: the members of the shares it is in, by the name each goes by there, each with the Markdown that mentions them (docs/NOTIFICATIONS_CONTRACT.md section 1) |
| `get_metanode` | what a metanode shows (docs/METANODES_CONTRACT.md): its source's forest as an indented outline in the tree's order, each line with its link, under a sentence saying what it is; a node with no viewer is read as its children |

Write:

| Tool | What it does |
| --- | --- |
| `create_node` | a child of a node, with a title and optional Markdown content |
| `append`, `insert_after`, `replace_section`, `replace_text`, `replace_block`, `remove_section`, `replace_content` | as above |
| `split_note` | split a text note by its headings of one `level` (default: the highest level it has) into child notes, last among its children and in order, each titled with its heading and holding the section under it; the note keeps the rest, and with `links` a list of links to the new notes where the first section was (below) |
| `rename`, `set_tags` | the node's title and tags |
| `move_node` | within a store, or between stores (`transplantNode`); planned first with `pimble_core::plan::plan_move` through `pimble-api`, so it refuses what the explorer refuses with the same sentences (into itself, read only, a mount whose store is not open, beside a store) before sending, says the cross-store and share-leaving warnings in its answer, and takes several nodes with `nodes` (one `moveNodes`, all or none) |
| `delete_node`, `undelete_node` | tombstone, put back |
| `set_metanode` | make a node a metanode shown with `viewer` and, with `depth`, a source of its subtree to that depth (1 its children); no viewer makes it an ordinary node again, its source kept. One `applyEdit` of the `metanode:*` keys |
| `list_viewers` | every viewer installed on this computer, from each plugin's guide (docs/AGENT_GUIDES_CONTRACT.md, read from the `pimble-guide` section of its `.wasm` in the plugins folder): what it shows and how it reads the tree, the node fields it reads, its settings with their schema and an example; Pimble's own `list`; plugins installed without a guide |
| `get_viewer_settings` | a metanode's settings for its viewer (`data.<viewer>`) as JSON, `{}` when none; a viewer other than the one it is shown with is refused, naming that one |
| `set_viewer_settings` | write a metanode's settings whole, after checking them against the viewer's schema; a refusal names the place and what was wanted, and nothing is written. One `applyEdit` of `data.<viewer>` |

`get_node` describes a metanode in one line ("a metanode: the children of Work, two levels,
shown with the kanban viewer") and says when a newer Pimble wrote part of its source.

Metanodes with a query (docs/QUERY_METANODES_CONTRACT.md section 7, built 2026-11-04): `set_metanode`
takes `query` instead of `depth` (`within`: omitted for the node's store, "everywhere", a
store's name, or a node for everything under it; `all`: conditions, each one of `tag`,
`no_tag`, `text`, `field` with `is` and `value`, `links_to`, `type`; `order`: "title",
"modified", "created" or "field:<name>"; `descending`; `limit`, 1 to 500, default 50), and
the viewer is "list" unless one is named. `get_metanode` lists the node's own children, then
"Found (n; ...)" and each find as `- Title (link) [found, in Store / Parent]`: the real node,
which `move_node` and `delete_node` act on where it lives.

Sheets (docs/SHEETS_CONTRACT.md section 10, wave 1; built 2026-10-19 on branch `sheets`).
`pimble-mcp` links `pimble-sheet` and computes what it reads exactly as the app does, over
the node documents it fetches (`crates/pimble-mcp/src/sheets/`: a `Book` filled with the
home node, the notes its stored formulas name, and the open stores' titles when a formula
names or shows another note). Every write is one `applyEdit` of the update the
`Sheets::edit` transactions made, so it touches only what it names. Formulas go in and
come out as people write them (names), turned into ids with `to_stored` as the formula
editor does. Tables, columns and rows are named the way people name them (a row by its
id or its first column's value); an ambiguous name is refused with the candidates.

| Tool | What it does |
| --- | --- |
| `list_tables` | a node's tables, a store's (`store`), or the tables whose formulas read a given one (`reads`): name, kind, row count, columns with types, the node they sit in |
| `get_table` | columns (type, link target, formula, format) and rows as a Markdown table, typed values as typed and computed values as shown, each error's code and sentence; `filter` (column = value) and `limit` |
| `evaluate` | a formula run in a note's context (and a table's, and one row's) without storing it |
| `sheet_formula_guide` | the guide "Sheet formulas", also published as the MCP resource `pimble://guides/sheet-formulas` |
| `create_sheet` | a sheet node under a parent, its first table, columns and rows written in the edit that creates it (`createNode` with content, as the app's "New Sheet") |
| `create_table` | a table in a sheet node with columns and rows; in a text node it is refused until wave 2 |
| `add_rows`, `update_rows`, `delete_rows` | values as typed (`"$12.50"`, `"yesterday"`, `"Civic"` for a link), parsed by the column's type; a value that does not parse or a link that matches no row is refused unless `keep_as_typed`; an update writes only the cells that change |
| `add_column`, `update_column`, `delete_column` | name, type, link target, formula, format; a type change carries values over as typed; a formula that does not parse, names something that does not resolve, gives `#SPILL!`, `#CIRC!` or `#NAME?` is refused, and an accepted one answers its first values |
| `set_summary` | a summary's formula (made when no table has the name) and its result columns' names, checked the same way |
| `rename_table`, `delete_table` | a table of the node; the answer names the formulas and link columns that read a deleted table or column |

`get_node` shows a node's tables as Markdown tables after its content (a sheet node's
content is its tables). Not yet: link-to-a-node columns, tables in text and `move_table`
(waves 2-3), and the later waves' `get_fields`, `set_fields`, `rows_pointing_here` and
`paste_table`.

### Canvases (built 2026-10-29, branch `mcp-canvas`)

Joe, 2026-10-29: "We need MCP tools for everything." A canvas (a board in the code,
docs/IMAGE_MARKUP_CONTRACT.md section 3) is read and written through `pimble-board`, the
crate the app draws with, in `crates/pimble-mcp/src/tools/canvas_tools.rs`. A canvas is
named by `node` (a canvas node, or the `pimble-board:` address of an embedded one) and, for
a picture's canvas in a note's text, `canvas` (the id after `#canvas=`, or the whole URL).
Every write is one `BoardTxn` over the canvas as it is in the document fetched for the
write, sent with `applyEdit` (`Pimble::edit_doc`); a refusal anywhere in a call writes
nothing. Elements are stamped with the agent's id and name (`whoAmI`), as a person's are
with theirs, and after each call the agent is present on the canvas with its pointer on
what it touched and that selected (`Presence::say_on_board`).

| Tool | What it does |
| --- | --- |
| `get_canvas` | page, what shows, layers top first, and every element: id, kind, text, geometry in canvas units, transform, set style keys, frame or group, lock, author (400 elements a page; `offset` and `limit`, at most 2000, page through them, and the "and more" line names the next offset; `layer` lists one layer's, a group's every layer's; each layer heading gives its element or operation count and its approximate size, and the canvas its document's size, "Large canvases" below). `get_node` on a canvas node shows the same |
| `render_canvas` | the canvas as a PNG (all of it, an `area`, or `elements` with their members), flattened onto white, longest side `max_size` (1024, at most 2048), with a sentence mapping pixels back to canvas units; pictures fetched through `getBlob`, card titles live; `show_all` shows hidden layers and markup |
| `create_canvas` | a canvas node under a parent, empty ("Canvas", chalkboard icon) or from a picture file (its name, photo icon, the picture locked in a layer "Picture" under "Layer 1"), with first elements, written in the `createNode` that makes it, as the app does |
| `insert_canvas` | a canvas into a note's text after a quoted block (or at the end): a picture file, a `pimble-blob:` picture, or empty; the canvas and the picture naming it in one edit (the app needs two, since its picture lives in the editor) |
| `add_elements` | any kind (sticky, text, rect, ellipse, diamond, polygon, star, path, frame, line, arrow, connector, pen, highlighter, step, callout, card, picture, spotlight, group) with text, style, rotation, layer, frame or group membership and lock; `ref` names an element within the call, so connectors, parents and a group's `members` can point at elements made in the same call; connectors are added after the rest whatever their place in the list; picture `file`s are stored as blobs first (`pimble_image::fit`, as the app) |
| `update_elements` | per element, only what is given: geometry merged into what it has, points, a path's `d` and `rule`, text (smallest edit), style keys set or cleared, `move_by` and `rotation` (its transform, so a frame's members move with it), layer, parent, stacking order, lock |
| `delete_elements` | by id |
| `add_layer`, `update_layer`, `delete_layer` | name, visibility, lock, opacity, order, `blend`, `parent` (the group it is in; `""` in an update moves it to the top level), `clip` (an element id; null or `""` in an update releases it); `add_layer` takes `kind` (`vector`, the default, `paint` or `group`); delete with its elements (a group with everything in it) or `merge_down` (a group: Merge Group) |
| `set_canvas` | a page (or one fitted to what shows, or none) and the markup switch |

Added the same day, from sketching a floor plan and a game level with contour lines
through the tools: strokes take `smooth` (a Catmull-Rom curve through the given points,
eight steps a segment, stored as ordinary points) and `closed`; a pen with a `fill` is a
filled freehand shape (rendered, exported and hit as one, `pimble-board`; the app's style
panel does not offer it yet); a text label takes `center` (x, y its center); `get_canvas`
gives text and grown sticky notes the size their text takes.

**Layer kinds, blend modes and groups** (docs/DRAWING_CONTRACT.md sections 1 and 5, built
2026-11-14): `get_canvas` lists the layers top first as a tree, a group's heading (`## Group
"Name" (id)`) before the layers it holds, each level one `#` deeper; a layer's line says its
group, a blend that is not its kind's default, and "hidden by its group" or "locked by its
group" when a group's flag is what hides or locks it; a paint layer reads `Paint layer` and
holds no elements (it holds brush strokes, the `paint` tool's), so elements named into it or a group are
refused with a sentence, and with no layer named they go to the top vector layer that is not
locked and not inside a hidden or locked group. `update_layer` with `parent` moves a layer
into a group (on top unless `order` says where); a group is never moved into itself or a
group it holds. Merging down onto a paint layer, and merging a group that holds a paint
layer, draw what merges as pixels (wave 5, "Painting" below); a paint layer merging down onto
a vector layer is refused with a sentence.

**Clip masks** (docs/DRAWING_WAVE6_CONTRACT.md section 1, built 2026-11-18): `add_layer` and
`update_layer` take `clip`, the id of a closed shape (rect, ellipse, diamond, polygon, star,
frame) or a closed path of the same canvas; everything the layer or group draws, its
elements and its paint, then shows only inside that outline, and inside every clip of the
groups above it too. In `update_layer`, `clip` null (or `""`) releases it and leaving it out
keeps it. Refused with the board's sentences: not closed ("Only a closed shape or a closed
path can clip a layer: ..."), no such element, inside the layer or group it would clip, and
a locked layer. The clip element still draws where it is, so the usual way is a hidden
"Clips" layer holding the clip shapes. `get_canvas` says `clipped to <id>` on the layer's
line, or `clip <id> not in effect (its shape is gone | is not closed | is inside the layer
it clips)` when the key names something that cannot clip. `export_canvas` writes a
`<clipPath>`; `render_canvas` draws the clip; Merge Down of a clipped layer keeps only what
showed, and `flatten_layer` keeps the layer's clip as it is (the pixels stay whole).

**Paths, live shapes and stroke detail** (docs/DRAWING_CONTRACT.md sections 2 and 5, built
2026-11-16): `add_elements` takes kind `path` with its outline as SVG path data (`d`: `M L H
V C S Q T A Z`, absolute or relative, several subpaths; quadratics and arcs become cubics)
and `rule` (`nonzero`, the default, or `evenodd` for holes); kinds `polygon` and `star` fill
the box `x, y, w, h`. The style takes `cap` (`butt`, `round`, `square`), `join` (`miter`,
`round`, `bevel`), `miter`, `dash` as a preset or a custom pattern of lengths (`"8 4"`), and
the live shapes' parameters: `radius` (rect), `sides` (polygon, 3 or more), `points` and
`inner` (star); `clear_style` clears any of them. `get_canvas` writes a path as `d="..."`
(with `, rule evenodd` when it has holes; a path whose data passes 4,000 characters is
summarized with its anchor count and extent), a polygon's sides and a star's points even
when they are the defaults, and the new style keys as set. `update_elements` takes `d` and
`rule`: on a path they replace its outline or rule, `x, y, w, h` move and stretch its
extent; `d` on a rect, ellipse, diamond, polygon, star, a line without arrow heads or a pen
stroke turns it into a path first (same id, style and place, `BoardTxn::convert_to_path`),
and on anything else is refused with a sentence. A preset `dash` replaces a custom pattern.
`render_canvas` draws all of it.

**Combining, gradients, flip, SVG in** (docs/DRAWING_CONTRACT.md sections 2 and 5, built
2026-11-17): three tools and two style keys.

| Tool | What it does |
| --- | --- |
| `combine_elements` | `ids` (two or more shapes, paths, lines without arrow heads, pen strokes) and `op` (`union`, `subtract`, `intersect`, `exclude`): one path in the top-most operand's place, style, note and fields (subtract: the bottom-most minus everything above it, in the bottom-most's), keeping its id; the others deleted; `BoardTxn::combine`. Text, sticky notes and the rest are refused with a sentence naming them; so is a result with nothing left |
| `flip_elements` | `ids` and `direction` (`horizontal`, `vertical`), together about the center of all of them; text, sticky notes, cards, steps, callouts and frames move to their mirrored place unmirrored; a group flips what is in it; `BoardTxn::flip` |
| `outline_stroke` | `ids` (stroked shapes, paths, lines without arrow heads, pen strokes): each stroke (width, ends, corners, miter limit, dashes) becomes a filled path of its outline, filled with the stroke's color or gradient and with no stroke; an element with no fill or text becomes the outline and keeps its id, one with a fill or text keeps them, loses its stroke, and gets the outline as a new path right above it (the answer names each); one edit; `BoardTxn::outline_stroke` (drawing contract decision 9). Text and the rest, and an element with no stroke, are refused with a sentence |
| `import_svg` | `svg` markup or a `file`, placed with its top-left (or with `center`, its center) at `x, y`, scaled so its longer side is `size` when given, into `layer`: paths with fills, strokes and gradients, groups as groups, simple text; the answer lists each new id (and its group) and every sentence of what was left out (filters, masks, clip paths, pictures, patterns...); `pimble_board::svg_in::parse` then `BoardTxn::import_svg`, one edit |

The style takes `fill_gradient` and `stroke_gradient`: `{ "type": "linear" | "radial",
"from": [x, y], "to": [x, y], "stops": [{ "offset": 0..1, "color": "#rrggbb(aa)" }, ...] }`,
handles in the element's own box ([0,0] top-left, [1,1] bottom-right; defaults run left to
right, a radial one from the center to the right edge), two stops or more (an offset left
out spreads them evenly). A solid `fill` or `stroke` given in `update_elements` (and "none")
replaces a gradient; `clear_style` takes `fill_gradient` and `stroke_gradient`, and clearing
`fill` or `stroke` clears its gradient too. `get_canvas` writes a gradient in the style line
(`fill linear gradient from 0,0.5 to 1,0.5 (#ff0000 at 0, #0000ff at 1)`) in place of the
solid fallback; `render_canvas` draws it.

**Painting** (docs/DRAWING_CONTRACT.md sections 3 and 5, built 2026-11-17, wave 4): one
tool, and paint layers in the two that read.

| Tool | What it does |
| --- | --- |
| `paint` | `strokes`, each `preset` (`round`, the default, `pencil`, `ink`, `airbrush`, `marker`, `charcoal`, `watercolor`, `highlighter`), `color` (the brush's own when left out), `size` (canvas units at full pressure), `opacity`, `erase` (an eraser stroke) and `points` (`[[x, y], ...]`, each optionally `[x, y, pressure]`, pressure 1 when left out); into `layer` (a paint layer's name or id), or with `highlight_picture` (a picture's id) into the multiply "Highlights" layer above it (`BoardTxn::highlights_layer`), or with neither into the paint layer a person's brush would take (`BoardTxn::paint_layer_for(None)`: the topmost shown, unlocked one, else a new "Paint 1" on top); every stroke in one edit (`BoardTxn::add_stroke`); the answer names the layer and each stroke's id. A layer that is not a paint layer, an unknown brush and a malformed point are refused with a sentence, nothing written |

Wave 5 (built 2026-11-17):

| Tool | What it does |
| --- | --- |
| `paint` | also `fills` (after the strokes, in order), each `at` (`[x, y]`), `color`, `tolerance` (0 to 1, default 0.15: how far a pixel's channels may be from the one at `at`), `contiguous` (default true; false fills every matching pixel in the area) and `area` (`{x, y, w, h}`, at most 2048 on a side, containing `at`); without `area` the fill may cover what the canvas shows and what this call's strokes reach, grown by 64 units, with the layer's painting (`pimble_board::fill_clip`, the area the app's Fill computes from the view), so a fill outside every shape ends. `strokes` may be left out when `fills` are given. `BoardTxn::add_fill`; the answer names each fill's id too |
| `flatten_layer` | `layer` (a paint layer): its whole history becomes pictures, one per tile of at most 1,024 units on a side (2,048 pixels; empty tiles left out), each stored as a blob of the canvas's store, laid down in one edit (`pimble_board::rasterize_at` with `RasterJob::Flatten`, `put_blob` per tile, `BoardTxn::apply_raster`). `scale` (pixels per unit, 0.125 to 2, default 2) draws a very large layer at fewer pixels. Refused with a sentence when a picture it draws cannot be fetched, or it needs more than 256 tiles at that density (since 2026-11-18; before, anything past 4,096 units across was refused) |
| `delete_layer` with `merge_down` | onto a paint layer (a vector layer's shapes, or a paint layer, with its opacity in the pixels and its blend on the `pixels` operation), or a group holding a paint layer (one paint layer of the group's pixels, keeping the group's name, opacity and blend): drawn as pixels the same way (`RasterJob::MergeDown`, `RasterJob::MergeGroup`). A hidden layer is refused (it would merge as nothing); a paint layer onto a vector layer is refused |

Wave 6, pictures in paint layers (docs/DRAWING_WAVE6_CONTRACT.md section 5, built 2026-11-18):

| Tool | What it does |
| --- | --- |
| `paint` | also `pictures` (laid first, in order, so this call's strokes and fills go above them), each `src` (a `pimble-blob:` URL already in Pimble) or `file` (a picture file on this computer, fitted under the 20 MiB limit with `pimble_image::fit` as `add_elements` and `insert_canvas` take one), `x`, `y` (its top-left corner before rotation), `w` and `h` (canvas units; both left out, one unit per picture pixel; one left out, the picture's proportions) and `rotation` (degrees clockwise about its center). Each becomes `pixels` operations of the paint layer with its rectangle and, when turned, a `transform` (`BoardTxn::add_placed_pixels`): one for a picture up to 2,048 pixels on a side (a `src` is laid as it is; a `file` is stored as one blob of the canvas's store), else one per piece of a grid of at most 2,048 pixels on a side (`pimble_board::picture_pieces`: each piece its own blob, JPEG when opaque, PNG otherwise; the pieces share the transform), as Flatten tiles a layer. `strokes` and `fills` may be left out when `pictures` are given. Refused with a sentence, nothing written: neither or both of `src` and `file`, a `src` that is not a `pimble-blob:` URL or cannot be fetched, a file that cannot be read or is not a picture, a size of 0 or less, more than 256 pieces. The answer names each picture's operation ids |

Smudge, blur and moves of paint selections have no tool: an agent paints, fills and lays
pictures; the summary counts them when a person made them.

Wave 6, map lettering (docs/DRAWING_WAVE6_CONTRACT.md section 4, built 2026-11-18): new
style keys, taken by `add_elements` and `update_elements` (`style`, and `clear_style` by
the same names) and shown by `get_canvas` and `get_elements`:

| Key | What it does |
| --- | --- |
| `font` | `"sans"` (Noto Sans, the default), `"serif"` (Noto Serif) or `"condensed"` (Noto Sans Condensed), optionally followed by `bold` and `italic` (`"serif bold italic"`, `"condensed-bold"`); something else is refused with a sentence. Stored as three keys: `font`, `bold`, `italic` |
| `bold`, `italic` | booleans, over what `font` names |
| `halo_color`, `halo_width` | a halo behind the glyphs (white by default; with only a color, a tenth of the font size wide), canvas units |
| `tracking` | letter spacing in em (stored clamped to -0.5..2) |
| `along` | on a text: an element id or a ref of the same call (a path, line, arrow or pen stroke); the text is set along it (`BoardTxn::put_on_path`, refused with its sentence for anything else). `""` takes it off (`release_from_path`) |
| `along_offset` | 0..1 of the path's length; the text's `align` says which part sits there (default left 0, center 0.5, right 1) |
| `along_side` | `"above"` (default), `"on"` or `"below"` |

`get_canvas` reads them back in one phrase each (`typeface "serif bold italic"`, `halo
#ffffff 3`, `tracking 0.2`, `along <id> at 0.5 below`); `get_elements` gives them as the
keys above, so its JSON writes back verbatim (an `along` naming an element of the same
list resolves to its copy, as `copy_elements` does).

Wave 6, symbols and scatter (docs/DRAWING_WAVE6_CONTRACT.md section 2, built 2026-11-18):
a **symbol** is a few elements kept as a definition (`symbol:<id>` on the board), drawn only
where a **scatter** places it; a scatter is one element holding thousands of placements
(16 bytes each, packed: 25,000 trees are about 400 KB, where 25,000 dabs were 15 MB).

- `add_symbol`: `elements` (specs as `add_elements` takes them, in the symbol's own space:
  0,0 is where a placement puts it; no connectors, cards, spotlights or scatters), or `ids`
  of elements on the canvas (copied into the symbol, centered on what they cover, and
  replaced by one scatter placing it where they were, unless `keep`), or `elements` with
  `symbol` (an id or name) to add to one. Answers the symbol's id.
- `scatter`: `symbol` (id or name) and one of `placements` (`[x, y]` or `[x, y, scale,
  rotation]`), `region` (a closed element's id: rectangle, ellipse, polygon, star, frame,
  closed path) or `d` (path data), with `density` (per 10,000 square units) or `spacing`
  (units), `method` (`"poisson"`, the default, or `"grid"` with `position_jitter`), `seed`,
  `size`, `size_jitter`, `rotation_jitter` (degrees either way), `tint` (`#rrggbbaa`, the
  alpha how far the colors move), `tint_jitter`; `into` appends to a scatter, `layer` names
  the layer (a region's scatter goes just above the region otherwise). One call, one edit.
- `get_canvas` lists the symbols after the element count (`- symbol <id> "Oak": 2 elements
  (<id> rect, <id> ellipse), at -10,-18 size 20x28; 1 scatter, 1,415 placements`) and a
  scatter as `scatter 1,415 placements of symbol "Oak" within ...`; `get_elements` gives a
  scatter as `kind`, `symbol`, `placements` (the count) and `bounds`, never the placements.
- `update_elements`: a symbol's element ids (listed under the symbol) change like any
  element's, and every scatter of it with them; on a scatter, `symbol` (id or name) and
  the style key `tint` (`clear_style` `"tint"`), and `move_by` moves it all. A scatter's
  geometry is refused with a sentence (placements are added with `scatter`'s `into`).
- `delete_elements` deletes a symbol's elements by id; `copy_elements` and `copy_layer`
  bring a scatter's symbol along under its id (pictures inside a symbol are not copied
  between stores). `export_canvas` writes `<symbol>`s and a `<use>` per placement.

Wave 6, soft edges (docs/DRAWING_WAVE6_CONTRACT.md section 3, built 2026-11-18): a radius
in canvas units, the edge fading over about the radius on each side and reaching 1.5 times
it past the hard edge (a Gaussian with sigma half the radius).

- `add_elements` and `update_elements` take `feather` in `style` (any element; `clear_style`
  `"feather"`, or 0): the element's edge fades out, its inside stays sharp.
- `add_layer` and `update_layer` take `blur` (everything the layer or group draws softened:
  haze, mist, glow) and `feather` (the edge of what it draws faded, its inside sharp); 0
  removes either. A clip cuts after them.
- `get_canvas` says `feather 60` among an element's style and `blur 40, feather 12.5`
  among a layer's flags; `get_elements` gives `feather` in `style`; `copy_layer` and
  `copy_elements` keep them; `export_canvas` writes `feGaussianBlur` filters in canvas units
  (a feather as the renderer draws it: the sharp drawing inside times the blurred alpha
  plus the blurred drawing outside, with `feComposite`); `render_canvas` draws them;
  `flatten_layer` draws a paint layer's own blur and feather into its pixels and takes the
  keys off the layer, and a merge down draws them in.

`get_canvas` gives a paint layer one line under its heading, never the points: how many
brush strokes, eraser strokes, pictures (pasted, flattened or merged), fills, smudges,
blurs, selections moved and selections cleared, and operations of a later version it
holds, the brushes (`watercolor x3`), up to eight colors (strokes' and fills'), the area it
paints, and who painted (`1 brush stroke, 1 eraser stroke · brushes watercolor x1 · colors
#228be6 · paints the area at -9,-9 size 118x34 · by Claude`). `render_canvas` draws paint layers in their place among
the layers with their blend and opacity, and fetches the pictures `pixels` operations name
(`BoardSnapshot::picture_srcs`). `add_layer` with `kind` `"paint"` makes an empty one.

**Meaning** (Joe, 2026-10-29; docs/IMAGE_MARKUP_CONTRACT.md "Meaning"): `add_elements` and
`update_elements` take `note` and `fields` (in an update, a field given as null is removed),
`add_layer` and `update_layer` the same for layers; `get_canvas` prints each under its
element or layer and takes `with_field` ("role" or "role=door") to list only the elements
that have it. A change of only note and fields is allowed on a locked element. The server's
instructions tell an assistant to read meaning before guessing and to write it when what it
draws does not say what it is.

What the app's tools refuse a person these refuse an agent, judged by `pimble_board`
(`BoardTxn`, docs/API_CONTRACT.md wave 1): an element that is locked takes only a change of
its note and fields or one that unlocks it (in the same change), an element on a locked layer
takes nothing but note and fields, nothing is added to a locked layer, a locked layer is
neither deleted nor merged down, and with every layer locked a new element has nowhere to go.
`delete_elements` deletes what it can and keeps the locked ones, and its answer names them
(open decision 5). Search already indexes canvas text, so `search` finds sticky notes and labels.

**Large canvases** (built 2026-11-18, after an agent drew an illustrated county map, about
11.8 × 11 km in canvas units, with 1,347 vector elements and a paint layer of 25,000
single-point dabs; docs/DRAWING_CONTRACT.md "Large canvases"):

| Tool | What it does |
| --- | --- |
| `get_elements` | `ids` (a group's or frame's members come with it) or a `layer` (a group: every layer in it), paged with `offset` and `limit` (200, at most 1000): each element complete as JSON in the shape `add_elements` takes, bottom to top: `kind`, geometry (`x, y, w, h`; a line's or pen stroke's `points`, a pen's with `[x, y, pressure]`; a path's `d` and `rule`; a callout's `tip`; a connector's `from`/`to` or `from_point`/`to_point` and sides; a card's `node`; a picture's `src` and `crop`), `text`, the whole `style`, `transform` (`tx, ty, rot, sx, sy`, when not the identity), `layer` (its name), `parent`, `locked`, `note`, `fields`, plus `id` and a `ref` equal to it, so a list passed back to `add_elements` keeps its groups and connectors. A paint layer answers `{ "strokes": [...], "fills": [...] }` in the shape `paint` takes (preset, color, size, opacity, erase, points with pressure; at, color, tolerance, contiguous, area), and counts the operations `paint` cannot take (pictures, smudges, blurs, moves) |
| `export_canvas` | the canvas, one `layer` (its elements only) or an `area` as an SVG document (`pimble_board::render_svg`: shapes as paths, text as outlines, pictures and, without `layer`, paint layers embedded as PNG), answered as text up to 1 MB or written to `file` |
| `copy_elements` | `from` (and `from_canvas`) `ids` (with their members) or a `from_layer`'s elements, `to` (and `to_canvas`, the same canvas allowed) into `layer` (made when missing; left out, a layer of each source layer's name), moved by `offset` `[dx, dy]`: new ids; parents, groups and connectors among the copies mapped to the copies, an end on an element left behind made a point where it was; notes, fields and locks kept; pictures of another store stored again in the target's (a blob belongs to its store); a paint layer's operations of every kind copied too (moved by the offset; a move's transform conjugated so it moves the same pixels); one edit of the target |
| `copy_layer` | `layer` (a vector layer with its elements, a paint layer with its whole history, a group with every layer in it, nested) from `from` to `to`, on top of the target's layers, as `name` (left out: its own, with " copy" on the same canvas), with blend, opacity, visibility, lock, note and fields, moved by `offset`; one edit |

`add_elements` takes `transform` (`{tx, ty, rot, sx, sy}`, over `rotation`), a picture's
`crop`, and a pen's points with their pressure (`[x, y, pressure]`), so what `get_elements`
gives goes back unchanged (tested: every kind read, written to another canvas and compared).
`paint`, `add_elements`, `import_svg`, `copy_elements` and `copy_layer` end their answer with
one more sentence when the canvas's document passes 8 MB (its size as fetched plus what the
write sent): it loads slowly everywhere, versions before 0.27 cannot load one past 10 MB,
and `flatten_layer` (or splitting the canvas with the copy tools) is the way out. The layer
sizes `get_canvas` prints are each layer's share of the document, estimated from what it
holds (a single-point brush dab measured at about 600 bytes stored) and scaled to the
document's real size for a canvas node. `list_stores` does not give a store's size (the
server does not keep one; adding it would read every document).

A message too large for the connection (a single document past 512 MB, now that the limit
is not 10 MB) is a sentence naming the document and the way out (`flatten_layer`, or
splitting), never jsonrpsee's "Response is too big" (`pimble_client::too_big_sentence`).

Not built: the spotlight/crop tools beyond what `add_elements` writes, "Convert to Node"
and "Flatten into New Canvas", and moving a picture's canvas between notes.

Not exposed, on purpose: hosting a store (`cloudHostStore`), sharing, relaying, linking to
a remote, closing or removing stores, signing in or out. Nothing is hosted unless the
person asked for it (CLAUDE.md), and an LLM asking is not the person asking. Signing in
stays in the app's Account menu; when no account is signed in, the cloud tools answer
"Sign in from Pimble's Account menu first."

### Reading part of a note, and where a search hit is (2026-11-18)

Measured over about 1,300 calls of another project's agents: long notes were read whole to
find one section, search hits did not say where in a note they matched, a query that was a
heading's exact words ranked that note last, and long notes were split by hand.

- **`get_node` reads part of a note.** `outline: true` answers the metadata and the
  headings only; `section` answers one heading and its section (`NodeDoc::section_markdown`
  over `markdown::find_heading`); `blocks` a range of top-level blocks
  (`NodeDoc::blocks_markdown`). Tables and discussions in that part are shown where they are,
  as in a whole read. A whole text note over 15,000 characters starts with a line saying its
  size and how to read less; it is never cut.
- **Hits say where.** `SearchHit::section` (pimble-search, and `SearchResultItem::section`
  over RPC, empty and left out when there is none) is the heading path above the match. The
  index keeps each node's headings: `Node.headings` (one per line, `@fulltext`, english) and
  `Node.outline` (block, offset in `text`, level and text per heading, from the node's
  `IndexUnit`s). A keyword hit is placed where the query is in the text (the whole query,
  else its first word that is not a stop word, which also moves the snippet there); a
  semantic hit at its chunk's first block (`b:N`).
- **Exact heading matches rank well.** The keyword query runs over `headings` too (weight
  1.5 beside title 2 and text 1), and among the candidates a note whose title is the query
  (`markdown::loose`) comes first, then one with a heading that is, then the rest in score
  order, so the app's search gets it too. The test (`pimble-search/tests/headings.rs`): six
  short notes that repeat "county" and "edges", and a long note with the heading "The
  county's edges" among eleven; before, the long note ranked 7th of 7 for "The county's
  edges", after, 1st, with `in: Design > The county's edges`, for that query in any case,
  without the apostrophe, or with a curly one. **The schema changed**, so every store's
  index is rebuilt once when it opens with this version (the `schema_hash` check in
  `open_index_for_store`).
- **The last store first.** Without `store`, `search` still searches every open store (Joe,
  2026-11-18, option a), but the hits of the store this MCP process last read or wrote in
  (any tool that named a node or a store in it: `Pimble::resolve`/`find_store` in
  pimble-mcp, in memory only) come first, all of them before the others', each group in its
  own score order; the answer's first line says so ("Hits from Family Management first (the
  store this session last worked in); pass `store` to search one store."). Until a tool
  names a store, and when that store has no hits, the ranking is unchanged and nothing is
  said.
- **`split_note`** (`Pimble::split_by_headings` in pimble-api, `NodeDoc::split_parts` and
  `cut_parts`): every section is judged first on a copy (a section that cannot be written
  writes nothing anywhere); then per section a `createNode` and an `applyEdit` that writes
  its Markdown (the canvases' and sheets' `data` copied with it), then one `applyEdit` of the
  note taking the sections out (and their canvases' and sheets' `data`) and putting the link
  list where the first was. The note keeps its id and link. Content moves as Markdown, so
  what Markdown cannot say about a block does not move, and a discussion anchored in a moved
  section stays on the note. Tested on ten sections with tables, code, nested lists, quotes,
  rules and subsections: each child's Markdown is byte for byte the section's before the
  split, and the Markdown it was written with.

### Naming a node

Every tool that takes a node takes one string, `node`, which is any of:

- a `pimble:` link (`pimble:<store>/<node>`, what the explorer's "Copy Link" puts on the
  clipboard), anchor ignored;
- a bare node id (a UUID), looked up in every open store;
- a path of titles from a store's name, `/`-separated (`Family Management/MEDICAL/Dr
  Smith`), matched case-insensitively, through mounts;
- since 2026-11-02, a path that starts anywhere (`MEDICAL/Dr Smith`), a run of titles down
  the tree as a sheet formula's note path is (docs/API_CONTRACT.md open decision 5). A path
  whose first title is a store's name is walked from it first.

A link is followed where its node went (`became`, after a move out of a share or between
stores), as the app follows one; before 2026-11-02 a moved node's old link "named no
node". A link to a deleted node says so.

Ambiguity is an error, never a guess: a path or id that matches several nodes answers
each candidate's id and path, so the LLM (or the person) picks one. Every node a tool
returns carries its id, its `pimble:` link and its path, so anything listed can be named
in the next call. A node's id is shown in the app too: its row's "Copy Link" gives the
link, which is what a person pastes into the LLM's chat.

## On `pimble-api` (2026-11-02, docs/API_CONTRACT.md wave 2)

The tools are words over `pimble_api::Pimble`: naming (`resolve`, `path`), the one write
(`edit`), recipes (`create_node`, `create_sheet`, `create_canvas` and `start_discussion`
each make their node with its first content in one `createNode`), identity (who an
agent's canvas elements and messages are signed by, asked once), presence frames, and
the sheet book (`Pimble::sheet_book`, over held documents). `crates/pimble-mcp` lost
`nodes.rs`, `presence.rs`, `sheets/book.rs` and `sheets/load.rs`. What changed for an
assistant:

- `create_node` writes its content in the create (before, a create and then an edit).
- An old link is followed where its node went; a path may start anywhere (above).
- A comment-only node refuses an edit before it is sent, with
  `StoreAccess::COMMENT_ONLY_REFUSAL`; `get_node` says `access: comment-only`.
- A discussion started on a node carries the `messages` icon, as the app's does.
- Everything made under a mount goes to its source: notes, sheets, canvases, discussion
  nodes, discussions on a mount, and `move_node` into a mount.
- Walks (`find_node`, `list_tables` of a store, `rows_pointing_here`, a formula's
  names) hold the store's documents once and are lookups after that.
- `rows_pointing_here` finds its tables through `Pimble::rows_pointing_here` (pimble-api,
  2026-11-04), the walk the app's Properties panel reads too, kept per store until a
  change touches it; what the tool answers is unchanged
  (docs/PROPERTIES_CONTRACT.md section 9).

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
- Marking or attributing LLM edits; an undo for them (the options are under "Recovery: an open question for Joe").
- A write scope narrower than the person's (an allowlist, or read-only mode).
