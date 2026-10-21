# Agents contract: connecting AI apps to Pimble, with access you choose

Status: **proposal, not approved.** Written 2026-10-20 from Joe's request: "something
in-app that makes it easy for users to configure MCP for their local agents, e.g. Claude
Desktop, GPT, etc. We need granular control for which node-trees MCP has access to, and I
suspect it needs to be handled differently for local MCP vs Pimble Cloud." The questions
for Joe are at the end; nothing here is built.

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

### "Everything" stays possible

An agent can be granted whole stores, or "Everything open on this computer" (today's
behaviour, the `Service` principal minus lifecycle). Existing setups, which launch
`pimble-mcp` with no key, keep working as "Everything" until the person picks
otherwise; the Agents window lists that as an agent named "Unrestricted (set up by
hand)" with a button to replace it with a scoped one.

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
  It stores nothing and can read nothing (the tunnel carries TLS to the relay face).
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
     none, read, edit. A row's choice covers its subtree unless a row inside says
     otherwise (a project readable, its Drafts folder editable). Stores are rows too.
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
2. **pimble-mcp**: `--agent` / `PIMBLE_AGENT_KEY`; no key stays unrestricted.
3. **App**: the Agents window, the tree picker, config writing for Claude Desktop,
   Claude Code and Cursor, the copy-paste fallback; desktop and web.
4. **Remote**: Streamable HTTP on the relay face, `/mcp/<id>` and OAuth in the accounts
   service, the offline sentence. Last, since it touches the hosted service and deploys.

## 8. Questions for Joe

1. **Is "an agent is a scoped collaborator, enforced by the server" the right model?**
   The alternative is a filter inside pimble-mcp, which is less code but trusted to
   pimble-mcp and easy to get wrong in one tool.
2. **Remote agents through the relay (section 4): now, later, or never?** They need the
   computer on, an OAuth flow in the accounts service and a deploy.
3. **Default for today's unrestricted setups**: keep them working as "Unrestricted"
   (proposed), or require everyone to create a scoped agent after updating?
4. **Roles**: reader and editor only (proposed), or also "can create nodes but not
   change existing ones" (an inbox-style agent)? Roles are Joe's call (memory: ask
   before changing the roles model).
5. **Writing other apps' config files** directly (with a backup), or copy-paste only?
