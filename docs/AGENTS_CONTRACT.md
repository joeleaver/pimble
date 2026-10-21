# Agents contract: connecting AI apps to Pimble, with access you choose

Status: **decided 2026-10-20, not built.** Written that day from Joe's request: "something
in-app that makes it easy for users to configure MCP for their local agents, e.g. Claude
Desktop, GPT, etc. We need granular control for which node-trees MCP has access to, and I
suspect it needs to be handled differently for local MCP vs Pimble Cloud." Joe's
decisions are in section 8.

Builds on `docs/MCP_CONTRACT.md` (pimble-mcp, its tools, the CRDT write path) and the
sharing model in `docs/NODE_DOCUMENT_CONTRACT.md` section 5 (scoped grants, `Reach`).

## 1. What is wrong today

- **Setup is by hand.** A person edits Claude Desktop's or Claude Code's JSON config,
  finds the path to `pimble-mcp`, and gets it right per OS (`docs/DEPLOY.md`).
- **Access is everything.** `pimble-mcp` connects as the local server's `Service`
  principal (MCP contract decision 3): every open store, every node, read and write,
  and the store lifecycle RPCs. There is no way to keep an agent out of a journal, or to
  let it read a project without changing it.
- **Only local, stdio clients.** An agent that runs in someone else's cloud (ChatGPT's
  connectors, claude.ai's remote MCP, an agent on another machine) cannot launch a
  process on this computer, so it cannot connect at all.

## 2. The idea: an agent is a collaborator you share with

Pimble already has the exact model for "this party may read these subtrees and edit
those": a share. A share is a scoped grant `(store, root node, role)`; the server's
`Reach` judges every read, write, search hit and notification against it; refusals are
whole sentences. **An agent is a principal with grants of the same shape**, made and held
on this device, never on Pimble Cloud:

```
Agent {
  id:      uuid
  name:    String              "Claude Desktop", "Work GPT"; the person's choice
  key:     secret              what the agent presents; shown once, stored hashed
  grants:  [(store, root, role)]   root = a node, or the store's root for all of it;
                                   role = reader | editor
  created, last_used
}
```

- **Enforced by the server, not by pimble-mcp.** The server resolves an agent's key to a
  `Principal::Agent { id, grants }` and judges it with the same `Reach` code a share
  member gets. A scoped agent can only ever see what its grants reach, through any tool,
  search included; `pimble-mcp` holds no rules of its own.
- **Nothing else is reachable.** An agent never gets the store lifecycle RPCs
  (`createStore`, `openStore`, `closeStore`, `addRemoteStore`, cloud sign-in...): those
  stay `Service`-only. Its tools are the content tools.
- **Mounts** inside a granted subtree: the source must be granted too, as for a share
  member (anything touching a mount authorizes against the source store). The picker
  says so beside a mount.
- **Its edits are attributed**: client id `agent:<id>`, so notifications and a later
  "who changed this" say which agent (the MCP contract's "Later: marking LLM edits").
- **Stored on this device only**: `<config dir>/pimble/agents.json` (0600), like
  `credentials.json`. Agents are not synced, not hosted, not shared; another device has
  its own (Joe, 2026-09-18: "nothing is hosted unless the person asked").

### Every agent is scoped, and so is every setup

An agent can be granted whole stores, but there is no unscoped agent. `pimble-mcp`
started without an agent key refuses to connect (Joe, 2026-10-20: setups made by hand
stop until scoped), and every tool answers one sentence: "Pimble needs this app set up
as an agent: in Pimble, Account > AI Agents... > New Agent." The release that ships this
says so in its notes, and `docs/DEPLOY.md`'s hand-written config section is replaced by
the Agents window.

## 3. Local agents (stdio: Claude Desktop, Claude Code, Cursor, ...)

The client launches `pimble-mcp --agent <id>` with the key in the environment
(`PIMBLE_AGENT_KEY`), never on the command line. `pimble-mcp` connects to the local
server presenting it. The embedded server is tokenless on loopback today, so the edge
change is: a connection that presents an agent key gets that agent's principal; one that
presents nothing stays `Service` (the app itself, the CLI).

That last point is the honest limit: **on one computer, scoping protects against an
agent overreaching, not against hostile local software.** Any process running as the
person can open `127.0.0.1:7462` without a key, or read the store files directly. The
scope is a promise the server keeps to the agent you configured, which is what the
request is about (keep my assistant out of my journal; let it read, not edit, this
project).

## 4. Remote agents (through Pimble Cloud)

Pimble Cloud cannot run the agent's tools itself: hosted stores are end to end encrypted
and the keys are on the person's devices (CRYPTO contract). So a remote agent reaches
the **person's own computer** through the relay tier, the way a share member reaches an
unhosted store (`docs/RELAY_CONTRACT.md`):

- The local server serves MCP over **Streamable HTTP** on the relay face (the MCP
  contract's "Later" item), judged by the same agent principal.
- The accounts service gives the agent a public address, `https://pimble.app/mcp/<agent
  id>`, and pipes its connections through the existing relay tunnel to this computer.
  **Correction (2026-10-21): it could read what passes.** A share's traffic through the
  relay is end-to-end encrypted, because members decrypt on their own devices; a remote
  agent cannot decrypt, so this computer answers it in plain text, and the agent's HTTPS
  ends at pimble.app's edge. Pimble Cloud would see note content in transit (in memory,
  not stored). Joe chose to investigate keeping it blind first (section 9).
- The remote client authenticates with OAuth to Pimble Cloud (the usual way remote MCP
  connectors sign in; each client's exact requirements are checked when this is built); the consent screen names the agent and its grants. The accounts
  service mints a short-lived token naming that agent alone, as it does for a relayed
  store.
- **It works while this computer is on and signed in.** Off, the agent gets one
  sentence: "Pimble on <device> is offline; this agent works while it is on." This is
  the same rule shares from an unhosted store follow, and the same honesty.

Local and remote agents are the same object with the same grants; "remote" is a switch
on the agent ("Reachable from the internet through Pimble Cloud"), off by default, and
needs the account signed in.

## 5. The app: Account > "AI Agents..."

One window, both apps (web where it can: the browser has no local agents, so it shows
only remote ones and says why):

1. **The list**: each agent's name, what it reaches in a line ("Reads Work, edits
   Work/Drafts"), local or remote, last used. Rows open the editor; "Remove" revokes at
   once (its key stops working on the next call).
2. **New agent**:
   - **Which app**: Claude Desktop, Claude Code, Cursor, "Another MCP client (stdio)",
     "A remote client (ChatGPT, claude.ai)". This only decides the setup step; the list
     of apps Pimble configures itself grows as their config formats are confirmed.
   - **What it can reach**: the explorer's own tree with a three-way control per row:
     none, read, edit. A row's choice covers its subtree, and a row inside can only
     widen it (a project readable, its Drafts folder editable): an agent reaches the
     union of its grants, judged as a share's member is, so "edit the project except
     Drafts" is made by granting the parts, not by subtracting one. Stores are rows too.
     Nothing is checked to start with.
   - **Setup**: for an app Pimble knows, "Add to Claude Desktop" writes its config
     entry (section 6) and says to restart that app; for any other client, the JSON to
     paste and a Copy button; for remote, the address and "Connect" instructions.
3. **In the explorer**: rows an agent can reach can carry a small marker (off by
   default; View > "Show Agent Access"), so the person sees at a glance what is exposed.

## 6. Writing other apps' config

Pimble adds one `mcpServers.pimble-<agent name>` entry and touches nothing else:

| Client | File |
| --- | --- |
| Claude Desktop | Linux `~/.config/Claude/claude_desktop_config.json`, Windows `%APPDATA%\Claude\claude_desktop_config.json`, macOS `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Claude Code | `claude mcp add` if `claude` is on the PATH, else `~/.claude.json` user scope |
| ChatGPT desktop, Codex CLI and IDE extension | `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`): a `[mcp_servers.pimble-<name>]` table with `command` and an `env` table, merged with `toml_edit` so the file's comments and layout stay |
| Cursor | `~/.cursor/mcp.json` |

The entry names the installed `pimble-mcp` by absolute path (the self-updating install's
folder, so updates keep it working) and passes the key through `env`. The file is read,
the entry merged in, a backup written beside it (`.pimble-backup`), and the result
written atomically; a file that does not parse is left alone and the person gets the
snippet instead. Removing an agent removes its entry the same way.

## 7. Waves

1. **Server**: `Principal::Agent`, agent keys at the HTTP edge, `Reach` for agents
   (reusing the share judgement), lifecycle RPCs refused, `agents.json`, attribution.
   Tests: an agent reads and writes only inside its grants, through every tool and
   search; a reader is refused writes with the sentence; a removed agent's key fails.
2. **pimble-mcp**: `--agent` / `PIMBLE_AGENT_KEY`; no key refuses with the sentence.
3. **App**: the Agents window, the tree picker, config writing for Claude Desktop,
   Claude Code and Cursor, the copy-paste fallback; desktop and web.
4. **Remote**: deferred (section 9). Every major desktop client reaches Pimble locally;
   a public endpoint would expose plaintext to jkbase's edge and Pimble Cloud.

## 8. Decisions (Joe, 2026-10-20)

1. **Enforced by the server**: an agent is a scoped principal with share-shaped grants,
   judged by `Reach`; not a filter in pimble-mcp.
2. **Remote agents in the same piece of work**, through the relay, working while the
   computer is on.
3. **Setups made by hand stop until scoped**: no unrestricted agent, no key no access.
4. **Roles: reader and editor**, as share members have.
5. Not asked, the proposal stands until Joe says otherwise: Pimble writes the config of
   the apps it knows (Claude Desktop, Claude Code, Cursor) with a backup, and offers the
   snippet for everything else.

## Built in wave 1 (server), 2026-10-20

- `crates/pimble-server/src/agents.rs`: `AgentRegistry` (`agents.json`, 0600, keys
  kept as SHA-256 only, `pmbl_agent_<id>_<secret>`), `normalize` (one grant per root,
  the higher role winning; a whole-store reader widened by roots inside becomes its
  root node), and `AgentConnection`.
- An agent is a `Principal::User` with `sub = agent:<id>` and its grants, so `Reach`,
  `authorize`, search filtering and the `Service`-only lifecycle RPCs judge it as they
  judge a share's member; `present_store_to` keeps the store's real name for an agent.
- The HTTP edge checks a credential shaped like an agent key before anything else (the
  tokenless embedded server would otherwise make it `Service`); an unknown key is 401.
- **Re-judged on every call**: the connection carries the agent's id, and
  `principal_of` rebuilds its principal from the registry, so removing or narrowing an
  agent holds an open connection to it on its next call.
- Agents load on a loopback server with no JWT verifier (the app's own) or where
  `ServerConfig::agents_path` says; never on a relay face or a hosted server.
- RPCs `listAgents`, `createAgent` (answers the key once), `updateAgent`, `removeAgent`,
  all `Service`-only; grants naming a store not open here or a node not in it are
  refused with a sentence. `last_used_at` is written at most once a minute.
- Tests: `tests/agents.rs` (reach through reads, writes, children, search, lifecycle;
  narrowing and removal on an open connection; grant checks), unit tests in the module.

## Built in wave 2 (pimble-mcp), 2026-10-20

- `PIMBLE_AGENT_KEY` in the client's environment (the `--agent` flag the proposal named
  is not needed: the key names its agent). Without one every tool answers
  `connection::NOT_AN_AGENT`, and no server is started.
- With one, `pimble-mcp` joins or starts the local server as before (its `Service`
  connection only opens the saved store list when it starts the server), then makes
  every tool call over a second connection that presents the key. A removed agent gets
  "This agent was removed in Pimble, or its key is wrong: set it up again in Account >
  AI Agents...". Edits carry `agent:<id>`.
- Paths: a store shown with one root as its own (an agent of one subtree) is walked from
  that root, as listings and search name its nodes (`nodes::resolve_path`).
- `pimble-cli agents`, `create-agent <name> <store>[/<node>]=<reader|editor>...`,
  `remove-agent <id>`; `docs/DEPLOY.md`'s client section now sets up an agent.
- Walked through over stdio (`scripts/mcp/drive.py`) on a scratch copy of the family
  store: no key, the sentence; an agent reading MEDICAL lists and reads only MEDICAL,
  finds nothing in TAXES ("Turbo Tax"), is refused TAXES by link ("no grant for this
  document") and a write under MEDICAL ("You can read this, not change it.").

## Built in wave 3 (the app), 2026-10-20

- Account > "AI Agents..." (`crates/pimble-app/src/agents.rs`, desktop only): the list
  (name, reach in a line, last used, Edit, Remove), New Agent (name, which app, the
  picker), and a made agent's page (its key once, "Add to <app>", the snippet, Copy
  Setup). Commands `ListAgents`/`CreateAgent`/`UpdateAgent`/`RemoveAgent`, events
  `AgentsListed`/`AgentCreated`/`AgentsError` (shared `commands.rs`).
- The picker is the open stores' tree, loaded as rows open, with None / Read / Edit per
  row; a row nothing is chosen on says what it inherits ("reads, from above"). A store's
  root node stands for the whole store.
- "Add to Claude Desktop / Claude Code / Cursor" merges one `mcpServers.pimble-<name>`
  entry (`command` the `pimble-mcp` beside the app, `env.PIMBLE_AGENT_KEY`; Claude Code's
  with `type: stdio`), keeps the old file as `.pimble-backup`, writes by rename, and
  leaves a file it cannot parse alone (`write_entry`, tested).
- Walked through on an isolated app (port 7491, scratch config, fake `HOME`): an agent
  made with MEDICAL read and one folder inside it edited; the scratch Claude Desktop
  config got the entry; Edit loads the grants back; Remove takes it off the server.

**Not built yet, or known gaps:**
- View > "Show Agent Access" markers in the explorer.
- Removing an agent does not take its entry out of the app's config (the key stops
  working at once; the stale entry answers with the removed-agent sentence).
- The browser shows no Agents window; it gets one with remote agents (wave 4).

## 9. Remote agents and Pimble Cloud's blindness (investigated 2026-10-21)

**jkbase cannot pass TLS through today.** Its edge terminates TLS for every request:
`docs/managed-rhypedb-design.md` ("the edge is HTTP-only (hyper `http1::serve_connection`),
no L4/SNI passthrough"), and the TCP-ingress design that followed
(`docs/managed-rhypedb-tcp-ingress-design.md` section 4) also terminates TLS at the edge
and demuxes by ALPN before relaying plaintext inward. The L4 work is UDP
(`docs/managed-l4-udp-ingress-design.md`). Wildcard certificates exist
(`docs/wildcard-domains.md`), but they are the edge's, not a tenant machine's. So any
remote agent reaching pimble.app is decrypted by jkbase's edge and then by Pimble Cloud.

Ways to keep Pimble Cloud (and jkbase) blind, none built:

1. **SNI passthrough in jkbase** (Joe's platform; a feature request, never an edit from
   Pimble's side): connections to `<agent>.agents.pimble.app` are spliced, still
   encrypted, through the existing tunnel to the owner's computer, which holds that
   name's certificate (issued by ACME DNS-01 through Pimble Cloud, which never holds the
   private key). The edge would route on the ClientHello's SNI without terminating.
2. **A passthrough relay outside jkbase**: a small SNI router on a separate host doing
   the same. New infrastructure to run and secure.
3. **The person's own tunnel**: Tailscale Funnel or Cloudflare Tunnel in TLS-passthrough
   mode, TLS ending on their computer; Pimble would only configure it. Needs an account
   with that service, and is not "it just works".
4. **Accept transit plaintext**, said plainly when a remote agent is turned on (the AI
   service reads the same content anyway); never stored or logged.

### Decided: remote agents are deferred (Joe, 2026-10-21)

Every major desktop client now reaches Pimble locally, with nothing leaving the
computer: Claude Desktop, Claude Code, Cursor, and the ChatGPT desktop app with Codex,
which run local stdio MCP servers from `~/.codex/config.toml`
(learn.chatgpt.com/docs/extend/mcp, checked 2026-10-21). Only the web versions
(chatgpt.com, claude.ai) would need a public endpoint, and that is not built.

An HTTPS web API instead of MCP would not change this: whatever a cloud-hosted agent
calls must be public, so either Pimble Cloud answers it (and must hold readable notes:
the keys, or a plaintext copy) or it forwards to this computer (the relay, decrypting at
jkbase's edge without passthrough). The question is where plaintext lives, not the
protocol. If remote agents are ever wanted, the choices are the four above, or a store
the person deliberately hosts readable by their remote agents (a product decision of its
own, outside end-to-end encryption).

## Built 2026-10-21

- ChatGPT / Codex in the Agents window: "Add to ChatGPT / Codex" writes
  `~/.codex/config.toml` (`write_toml_entry`, tested: comments and other servers kept,
  backup written, a file that does not parse left alone); the snippet is TOML.
