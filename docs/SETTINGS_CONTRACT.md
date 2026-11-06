# Settings, and the plugin library

Status: all four waves built 2026-11-06 on branch `settings` (sections 9 to 12). The
shape was agreed with Joe in conversation ("Seems right"), including the four
recommendations in section 1, and he answered section 7 the same day.

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
- `GET /api/v1/plugins/<file>` serves one of that release's plugin files, fetched once and
  kept by the service.

Both run on the same origin as the web app, so the browser needs no cross-origin fetch, and
neither needs an account: plugins are public code, not anyone's data.

**Install.**

1. The file is downloaded.
2. Its SHA-256 and its signature are checked against the compiled-in key (`pimble-crypto`,
   both targets).
3. It is loaded (ABI checked) and added as in section 3.

`<plugins dir>/library.json` (`localStorage` in the browser) records which installed plugins came
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
Pimble opened its own links only from inside itself, so wave 4 also made the operating
system hand `pimble:` links to it (section 12).
It opens Settings at that plugin's row with Install ready. Nothing is installed until the
person presses it. With no Pimble installed, the page says to download it first.

## 7. Joe's answers (2026-11-06)

1. **The General section:** "yes, your recommendations". It holds the theme for now. The
   renderer, semantic search and new-store defaults can join it when someone asks.
2. **Shares:** "I think shares needs to be in settings also, maybe? But not adding a
   share". Shares is a section of Settings that lists them; a share is still made from the
   node's Share....
3. **The library on the website:** "in-app only is fine for the first waves". Nothing on
   the site until wave 4.

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

## 10. Wave 2 as built (2026-11-06)

- **The index and the check** (`crates/pimble-plugin-host/src/library.rs`, both targets):
  - `Index` and `Entry` are `plugins.json` as section 4 shows it. An index in another
    format is refused with "The library is in a newer format than this Pimble reads."
  - `offer` says what a row offers: Install, Installed, Update, "Added from a file" (the
    library leaves it alone) or "Needs a newer Pimble" (another plugin ABI).
  - `verify_download` checks the length, the SHA-256 and the Ed25519 signature.
    `verify_signature` is now the one check for everything Pimble installs: desktop
    updates call it too. The key is `crate::RELEASE_KEY` (`update-key.pub`).
  - `matches` makes sure the plugin that loaded is the id and version the index listed.
  - `Provenance` is what `library.json` holds: which installed plugins came from the
    library, and at which version.
- **Publishing.**
  - A plugin is in the library when its Cargo.toml says `[package.metadata.pimble]
    library = true` with a `summary`. Today that is the kanban, todo list and timeline
    viewers; hello and stuck are not.
  - `pimble-plugin-index` (a binary of `pimble-plugin-host`, feature `index-tool`) reads
    `cargo metadata` and the built modules. It writes `<id>-<version>.wasm`, the
    screenshot when there is one, and `plugins.json`, each entry from the plugin's own
    manifest.
  - The Release workflow's `build-plugins` job runs it, and the release job signs every
    `.wasm` with the update key, beside the packages.
- **A plugin's version is its crate's version** (all `0.1.0` today). A release that does
  not bump it publishes the same file name, so nobody is offered an update. Bump a
  plugin's version when it changes.
- **The accounts service** (`crates/pimble-cloud/src/releases.rs`):
  - `GET /api/v1/plugins` is the latest release's `plugins.json`, or
    `{"v":1,"plugins":[]}` when the release has none.
  - `GET /api/v1/plugins/<file>` serves only the files that index names. It fetches each
    once and keeps it in memory, not on disk: the three plugins are about 24 MB, and a
    name never changes its bytes.
  - `GET /releases` no longer lists the library's files, so the download page shows
    packages only.
- **The app** (`crates/pimble-app/src/plugins.rs`):
  - Settings > Plugins has a Library below the installed plugins. Each row shows the name
    and version, the summary, what the plugin asks for, and its button or label.
  - Installed plugins say "From the library." or "Added from a file.".
  - Install and Update download the module and its signature, verify both, then load the
    plugin and check it is the one listed. Only then is it installed, the way "Add from a
    File..." installs one: on the desktop the folder write, the rescan and the swap; in
    the browser IndexedDB and the swap. Then `library.json` records it.
  - Add from a File and Remove clear the plugin's provenance.
  - Where the record is kept:
    - desktop: `<plugins dir>/library.json`;
    - browser: `localStorage` `pimble.plugins.library`, not IndexedDB, since it is a few
      bytes, like the approvals.
  - Where the library is read from:
    - desktop: `https://pimble.app/api/v1/plugins`, or `PIMBLE_LIBRARY_URL` (for the
      local stack and a stub);
    - browser: the page's own origin.
  - Screenshots are published but not shown yet.
- **Checked:**
  - `pimble-plugin-host` `library` tests (6), all passing:
    - the index reads back and names only its files;
    - a newer format is refused;
    - what each row offers;
    - only a download signed by the key, with the listed hash, verifies;
    - the plugin must be the one listed;
    - provenance.
  - `pimble-cloud` (80, with the real rhypedb-server):
    - `the_plugin_library_is_the_latest_releases` serves the index, the module with its
      type and the signature, 404s for a file the index does not name (another `.wasm`
      in the release, a package, `plugins.json` itself), and `/releases` lists only the
      package;
    - `a_release_without_a_library_offers_no_plugins`.
  - `pimble-app` 221 tests. The workspace and the browser build without warnings.
  - `pimble-plugin-index` run here on the three plugins built for `wasm32-wasip1`: about
    7.6 to 8.2 MB each, `plugins.json` as section 4 shows it.
  - In the release app on the scratch copy, against a stub library whose files were
    signed with a throwaway key:
    - the Library listed Kanban and Timeline as "Added from a file" and Todo List
      (moved out of the folder) with Install;
    - Install said "The download did not verify, so nothing was installed.", the log said
      why ("the signature does not verify"), and nothing reached the folder.
  - In the browser on the local stack, with the accounts service pointed at a stub GitHub
    release (`PIMBLE_CLOUD_RELEASES_BASE_URL`), signed in as a fixture account:
    - the same three rows, all with Install;
    - Install on Kanban was refused with the same sentence;
    - IndexedDB held nothing and `pimble.plugins.library` was unset.
  - **A real install** was checked with wave 3 (section 11), in a local build trusting a
    throwaway key. Only the Release workflow holds the real key, so the first install
    under it comes with the first release that publishes the library. Check it then, on
    the installed desktop and on pimble.app.

## 11. Wave 3 as built (2026-11-06)

- **When:** 20 seconds after the plugins first load, then every 6 hours
  (`schedule_update_check`, rinch's `set_timeout` on both targets).
- **What updates** (`updates_due`): an index entry whose `offer` is Update, which means:
  - the installed plugin came from the library;
  - the entry is newer than the installed version;
  - it is built for this Pimble's plugin ABI.

  A plugin added from a file, one not installed, and one needing a newer Pimble are left
  alone.
- **How:** through the same download, verification and install as the Update button
  (`fetch_and_install`). Nothing is asked.
- **What it is allowed carries over:** approvals are kept by plugin id. When a new
  version asks for more, `Approval::covers` fails on its manifest, so the pane asks the
  way a first use does.
- **A pane showing the plugin keeps the version it runs.**
  - While the update installs, its id is in `QUIET`. `swapped` then marks the panes
    showing it (`Shown.newer`) instead of stopping them.
  - The pane says "A newer version of Kanban is ready." with "Restart Plugin"
    (`Outcome::ShownNewer`, `MetanodeUi::newer`). Restart Plugin stops the old version and
    mounts the new one.
  - A pane that mounts the plugin afresh gets the new version.
  - An Update pressed by the person still stops and remounts every pane at once
    (section 3).
- **What is said:**
  - The row says "Kanban updated to 0.1.1." until Settings is next closed (row lines are
    now cleared on close, not on every show). The log says it too.
  - A failed update is logged and tried again at the next check.
- **Checked:**
  - `pimble-app` 222 tests, among them `only_newer_library_plugins_this_pimble_runs_update_on_their_own`:
    - from-file, not installed, newer ABI and current plugins are left alone;
    - the one library plugin with a newer version updates.
  - The browser builds without warnings.
  - In a release build on the scratch copy, built to trust a throwaway key for this check
    only. The real `update-key.pub` was restored at once, and `target/release` was built
    again with it. Against a stub library signed with that key:
    1. Removing the Kanban added from a file and pressing Install put
       `kanban-viewer.wasm` in the folder. `library.json` recorded `0.1.0`, and the row
       said "From the library."
    2. Apex Kanban then asked to be allowed, and after Allow it showed its board.
    3. The app restarted with Kanban 0.1.1 in the library. After 20 s the log said
       "updating kanban-viewer to 0.1.1" and then "Kanban updated to 0.1.1.", and
       `library.json` said `0.1.1`.
    4. The open board kept running under "A newer version of Kanban is ready." and
       Restart Plugin brought the board back on 0.1.1.
    5. Settings showed Kanban 0.1.1 "From the library." with its approval kept, and the
       row line "Kanban updated to 0.1.1."; Timeline and Todo List, added from a file,
       were untouched. The line was gone once Settings closed and opened again.
  - Not run in the browser: its download and refusal were checked in wave 2, and the
    update path is the shared code above.

## 12. Wave 4 as built (2026-11-06)

**The link.** `crates/pimble-app/src/links.rs` `AppLink` reads:
- a note's link, which follows it as a click does;
- `pimble:settings`, which opens Settings at the section chosen last;
- `pimble:settings/<section>`;
- `pimble:settings/plugins?install=<id>`.

An install link opens Settings at Plugins and marks the plugin's library row with a bar.
The row says what to press: "Press Install to add Todo List.", "Press Update to get
0.2.0.", or that it is installed already, added from a file, or needs a newer Pimble.
**Nothing is installed until the person presses the button.** An `install` names only a
plain id, and only at Plugins. Anything else is "Pimble does not know how to open ...".

**From the operating system to the window.**
- **Registration.**
  - Linux: the menu entry says `Exec=... %u` and
    `MimeType=x-scheme-handler/pimble;`, and the app runs `update-desktop-database` and
    `xdg-mime default pimble.desktop x-scheme-handler/pimble`, both best effort.
  - Windows: `HKCU\Software\Classes\pimble`, with `URL Protocol` and
    `"<app>\pimble.exe" "%1"`.
  - The installer writes the Windows keys for a new install (`[Registry]`, removed on
    uninstall).
  - An installed copy writes its own registration at start when it differs
    (`update::register_links`). That covers copies installed before 0.20 and copies
    updated in place, which never ran `--install` again. A copy that is not an installed
    one registers nothing.
- **The handoff.** `pimble <link>` (`main.rs`) asks this computer's server, with a 3 s
  limit, to hand the link to a window: `openInApp`, Service-only, answers `delivered`.
  - Windows listen through `subscribeAppLinks`. The one open longest takes the link, and
    the second process exits.
  - With no server, an older one, or no window listening (a server `pimble-mcp` started),
    the process opens a window itself. It opens the link 1.5 s after it connects, so the
    stores it names are open.
- **Raising the window** needs rinch's `focus_current_window()`
  ([joeleaver/rinch#2310](https://github.com/joeleaver/rinch/pull/2310), open). Until it
  merges, `app::bring_to_front` calls `show_current_window()`, which does nothing for a
  window already open, so the link's change happens wherever the window is. Switch to the
  new call when the pin moves to a rinch that has it.

**The web app.** `/app/?open=<pimble: link>` keeps the link in the tab's
`sessionStorage` (`pimble.open`), so it survives a trip through sign-in or unlock. Once
the app is up, it is opened the way the desktop opens a launch link.

**The website.** `site/plugins.html` lists `/api/v1/plugins`:
- each plugin's screenshot when the index names one, its name, version and summary, what
  it asks for, and its author;
- "Install in Pimble", which is the `pimble:` link;
- "Open in the web app", which is `/app/?open=...`;
- a line saying Install in Pimble needs Pimble on this computer, with a link to the
  download page. A browser cannot tell whether a scheme has a handler, so the line is
  always there.

"Plugins" is in the header (hidden on a phone, like "Web app") and the footer of the home,
download and plugins pages.

**Checked:**
- `pimble-server` `app_links`, all passing:
  - no window means `delivered: false`;
  - of two windows only the oldest hears the link;
  - when it closes, the next one does;
  - when both are gone, `false` again.
- `pimble-app` 223 tests, among them `settings_links_read_and_nothing_else_does`.
- The workspace with tests, the browser build, and
  `cargo xwin check -p pimble-app --release --target x86_64-pc-windows-msvc` (the
  registry code) build cleanly.
- In the release app on the scratch copy (its own port, its own XDG dirs):
  - **Handoff.** With the app running, `pimble "pimble:settings/plugins?install=todo-viewer"`
    exited in 15 ms. The window opened Settings > Plugins with Todo List saying "Press
    Install to add Todo List.", and nothing was written to the folder.
  - **Cold start.** Started with the same link and no Pimble running, it opened with the
    row marked.
  - **A note link** handed over opened Ides of March and selected it in the tree.
  - **Registration.** As a managed install in the scratch folder, with scratch
    `XDG_DATA_HOME` and `XDG_CONFIG_HOME`, it rewrote `pimble.desktop` with `%u` and the
    scheme, and `xdg-mime query default x-scheme-handler/pimble` answered `pimble.desktop`.
    Then `xdg-open "pimble:settings/keyboard"` reached the running window, which opened
    Settings at Keyboard. The real `~/.local/share/applications/pimble.desktop` and
    `mimeapps.list` were untouched.
- **The plugins page,** served beside a stub library: three rows with the right links, at
  1200 px and at 375 px, with no sideways scroll.
- **The web app** on the local stack: `/app/?open=pimble%3Asettings%2Fplugins%3Finstall%3Dtodo-viewer`
  in a locked tab went to sign-in. After signing in as the fixture account, it opened
  Settings > Plugins with Todo List marked and "Press Install to add Todo List.".
- **Not checked:** the window coming to the front (rinch#2310), and the registration on a
  real Windows machine.
