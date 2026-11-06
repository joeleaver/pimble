# Settings, and the plugin library

Status: wave 1 built 2026-11-06 on branch `settings` (section 9). The shape was agreed
with Joe in conversation ("Seems right"), including the four recommendations in section 1.
The open questions in section 7 are still his.

Pimble's preferences and account matters are spread over separate modals today: Account...,
Shares..., AI Agents..., Plugins..., plus View > "Toggle Dark Mode". Plugins arrive by hand:
`scripts/plugins/install.sh` builds them into a folder, or "Add plugin..." takes a `.wasm`
you already have. The desktop only reads that folder when it starts.

This contract makes three changes:

1. One **Settings** dialog, with sections.
2. Plugin changes that **take effect at once** on the desktop, as they already do in the
   browser.
3. A **plugin library**: the plugins pimble.app offers, installed with one click, kept up to
   date, the same on the desktop and in the browser.

## 1. Decisions

1. **Who publishes to the library: Pimble only, for now.** The library holds the plugins built
   from this repository's `plugins/` workspace (the kanban, todo list and timeline viewers,
   and whatever comes next). Anyone else's plugins can come later. They need a submission
   process, review and a way to take a plugin down, and none of that is designed here. The
   index format leaves room for them (an `author` on every entry).
2. **Every library plugin is signed.** CI signs each `.wasm` with the key that already signs
   desktop updates (docs/UPDATE_CONTRACT.md, "Signing"). The app checks the detached Ed25519
   signature against the public key compiled into it and installs nothing that does not
   verify. A plugin added by hand from a file still works. Settings labels it "Added from a
   file" and never updates it from the library.
3. **Library plugins update on their own.** A newer version that the running Pimble can load
   (the same plugin ABI) is installed without asking. What the plugin is allowed carries over
   to the new version. If the new manifest asks for more (another store, write where it only
   read), the pane asks again the way a first use does.
4. **Install happens in the app.** The library is a section of Settings. The website gets a
   page listing the same plugins (wave 4), and its "Install in Pimble" button is a pimble:
   link that opens Settings at that plugin. The site never hands out a file to install by
   hand.

## 2. The Settings dialog (wave 1)

`File > Settings...` and `Ctrl+,` on both targets. It replaces the Account..., AI Agents...
and Plugins... modals; their menu items open Settings at their section. It is one modal with
sections down the left and the chosen section's page on the right. The section you last
chose is remembered (`state.json` `settings_section`, `localStorage` in the browser).

| Section | What it holds | Desktop | Browser |
| --- | --- | --- | --- |
| General | Theme (dark, light). | yes | yes |
| Account | Today's Account modal: the sign-in form, or the email, the service URL, the switch that keeps what you have read in step across your devices (`personal_sync`: it is account sync, so it stays with the account), and Sign Out. Shares... stays a modal of its own, opened from the Account menu. | yes | the email, the same switch, "Your Account Page" and Sign Out |
| Plugins | Installed: each plugin's name, version, where it came from (the library, or added from a file), what it may do (today's approvals, with Revoke), and Remove. Below that, the Library (wave 2). "Add from a file..." stays. | yes | yes |
| AI Agents | Today's AI Agents modal, unchanged. | yes | no (the browser has no agents) |
| Updates | The running version, whether an update is downloaded ("Restart to update"), and the last check. | yes | no (the page is always the deployed version) |

Nothing moves out of the menus that people reach for daily. View > "Toggle Dark Mode" stays
as a shortcut for the same setting.

## 3. Plugin changes take effect at once (wave 1)

The browser already does this (`place::add`, `place::remove` in
`crates/pimble-app/src/plugins.rs`). The desktop does the same:

- **Add, replace and remove** from Settings. The plugin loads (or is checked and compiled) on
  a background thread. Every pane showing the old version stops it (`stop_everywhere`) and
  mounts the new one in place. The registry is swapped (`loaded`), which updates the
  explorer's "New Kanban"-style items. Remove also forgets what the plugin was allowed.
- **The plugins folder is watched** (`notify`, debounced 500 ms). A file added, replaced or
  deleted by `scripts/plugins/install.sh` or by hand is picked up the same way, so the
  script's "restart Pimble" line goes away.
- **What a pane keeps.** A plugin's settings and remembered state live in the node
  (`Props.settings`, `Props.remembered`), so a board comes back with its columns and choices.
  What the plugin held only in memory (a half-typed card name) is lost. For a change the
  person made, that is expected. For an automatic update (wave 3), a pane already showing the
  plugin keeps the old version until it is next mounted, and says "A newer version of
  Kanban is ready" with a "Restart plugin" button.

## 4. The library (wave 2)

**What CI publishes.** The Release workflow, on every `v*` tag, builds the `plugins/`
workspace (`wasm32-wasip1`, release) and attaches to the release, for each plugin:

- `<id>-<version>.wasm`
- `<id>-<version>.wasm.sig`, its Ed25519 signature
- `<id>-<version>.png`, a screenshot (optional: `plugins/<crate>/screenshot.png` when there
  is one)

It also attaches one `plugins.json` describing all of them:

```json
{ "v": 1, "plugins": [ {
  "id": "kanban", "name": "Kanban", "version": "0.19.1", "abi": 6,
  "author": "Pimble", "summary": "Columns of cards from a node's children.",
  "asks": { "stores": "chosen", "write": true },
  "wasm": "kanban-0.19.1.wasm", "sig": "kanban-0.19.1.wasm.sig", "shot": "kanban-0.19.1.png",
  "bytes": 1834221, "sha256": "..."
} ] }
```

Each entry is read from the plugin's own manifest, so the index never disagrees with what
the plugin says once it loads. A plugin whose version did not change between releases keeps
its file name, so the app sees nothing new to update.

**How the apps reach it.** The accounts service serves the library as it serves releases
(`/api/v1/releases`, cached from GitHub):

- `GET /api/v1/plugins` is the latest release's `plugins.json`.
- `GET /api/v1/plugins/<file>` streams one of that release's plugin files, cached on the
  service's disk.

Both run on the same origin as the web app, so the browser needs no cross-origin fetch, and
neither needs an account: plugins are public code, not anyone's data.

**Install.**

1. The file is downloaded.
2. Its SHA-256 and its signature are checked against the compiled-in key (`pimble-crypto`,
   both targets).
3. It is loaded (ABI checked) and added as in section 3.

`<plugins dir>/library.json` (IndexedDB in the browser) records which installed plugins came
from the library, at which version. A failure is said in a sentence on the plugin's row:
"The download did not verify, so nothing was installed."

**The Library list in Settings** shows every plugin in the index: its screenshot, name,
summary, version, and what it will ask for ("Reads and changes the stores you choose"). The
row's button reads Install, Installed or Update. A plugin the running Pimble cannot load (a
newer ABI) says "Needs a newer Pimble" and has no button.

## 5. Updates (wave 3)

When the app starts, and every 6 hours after, it reads the index. Each library plugin with a
newer version that the running Pimble can load is downloaded, verified and installed as in
section 4, quietly. Panes showing it follow section 3. A plugin added from a file is never
touched. Settings > Plugins says "Kanban updated to 0.20.0" on the row until the dialog is
next closed.

## 6. The website (wave 4)

`site/plugins.html` lists the same index from `/api/v1/plugins`: screenshot, name, summary,
what it asks for. Each plugin's "Install in Pimble" is `pimble:settings/plugins?install=<id>`.
Pimble already opens its own links (docs/LINKS_CONTRACT.md), so this needs one new link kind.
It opens Settings at that plugin's row with Install ready. Nothing is installed until the
person presses it. With no Pimble installed, the page says to download it first.

## 7. Open questions for Joe

1. **The General section.** Theme and the personal-state switch are what exist today. Should
   anything else go in now? For example: the renderer (GPU or CPU, which takes effect at the
   next start), whether semantic search is on, or the default for new stores.
2. **Shares... in Settings or apart.** I've kept it a modal of its own, because it is about
   stores rather than about this device. It could be a sixth section instead.
3. **The library on the website without the app.** Is a listing page wanted at all before
   wave 4, or does the library live only in the app until then?

## 8. Waves

1. The Settings dialog and plugin changes that take effect at once (sections 2 and 3), on
   both targets.
2. The library: CI publishing, the accounts service routes, install and verification, the
   Library list.
3. Automatic updates.
4. The website page and the `pimble:settings` link.

Each wave is checked as the plugin waves were: unit tests for the pure parts (index parsing,
the verify step, which version wins), a host test for a swap under a mounted pane, and the
release app on the scratch copy, with the browser checked on the local stack.

## 9. Wave 1 as built (2026-11-06)

- `crates/pimble-app/src/settings.rs`: `Section` (General, Account, Plugins, AI Agents,
  Updates; the browser has the first three), `AppStore::settings` (the section shown, `None`
  while closed), `open` (the section chosen last, `state.json` `settings_section`,
  `localStorage` `pimble.settings_section`) and `open_at`. Each page is rendered once and
  shown while its section is chosen. Choosing a section readies its page the way its old
  window was readied on opening (`plugins::shown`, `agents::shown`, `app::account_shown`);
  closing clears what lived only while open (a typed password, an agent key shown once).
- The Account, Plugins and AI Agents modals are pages now (`render_page`), and their menu
  items open Settings at them. `open_account_modal` (the status bar, "Host on Pimble
  Cloud..." when signed out) is Settings at Account with its hint. File > "Settings..."
  (`Ctrl+,`) on both targets. Updates shows what Help > About says, with Check Now and
  Restart to Update.
- **The desktop's plugins take effect at once.** `place::scan` reads the folder and
  compiles a file again only when its length or modified time changed, answering the ids
  whose plugin is new, replaced or gone. `swapped` stops each such plugin in every pane
  showing it, swaps the registry, and the panes holding metanodes mount again with what is
  loaded (the browser's add and remove go through `swapped` too). The folder is watched
  (`notify`, a burst is one rescan after 500 ms of quiet). "Add from a File..." checks
  that the picked file loads, then writes it over the file holding the same plugin (or
  `<id>.wasm`), beside it and renamed over it. Remove deletes the file and forgets the
  approval. `scripts/plugins/install.sh` copies the same way and no longer says to restart.
- **Checked:** pimble-app 219 tests; the workspace and the browser build. In the release
  app on the scratch copy: File > "Settings..." opened at General with the theme choice;
  Plugins listed the three plugins with Remove, Revoke and "Add from a File..."; moving
  `timeline_viewer.wasm` out of the folder turned the open calendar into "Shown as an
  outline: the calendar viewer isn't installed." and moving it back brought the calendar
  form back in place (about 7 s, most of it wasmtime compiling the timeline). `Ctrl+,`
  was not tried by hand: the debug driver's key presses do not reach menu chords (neither
  did `Ctrl+N`); rinch parses `Ctrl+,` (`"Comma"`).
