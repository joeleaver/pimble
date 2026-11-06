# What plugins tell agents: viewer guides

Status: designed and built 2026-11-06 on branch `agent-guides`. Joe asked for it the same
day ("We should probably build that"), after a look at what an agent could do with the
plugins through the MCP: it could guess its way to a Gregorian timeline but could not make
a calendar, set a timeline's calendar, or learn an invented calendar's month names.

## 1. The problem

A plugin's viewer reads two things an agent cannot discover today:

- **Node fields** on the nodes it shows. The timeline reads `date`, `start` and `end`. The
  todo list reads `done` (a timestamp) and `due`.
- **Its settings**, a JSON object in the metanode's `data` under the viewer's id. Examples:
  `data.timeline` (which calendar to count in, which fields to read, which calendars to
  show beside it) and `data.calendar` (a calendar's whole definition).

The MCP has tools for fields (`get_fields`, `set_fields`) but none for a node's `data`.
Nothing says which fields a viewer reads or what its settings look like, and the viewer
names themselves are only guessable.

## 2. Decisions (taken with the build; Joe's to overrule)

1. **A guide is data the plugin ships, never plugin code that runs for an agent.** Each
   plugin carries one JSON guide describing its viewers:
   - what each viewer shows and how it reads the tree;
   - the fields it reads, each with a sentence and an example;
   - its settings: a sentence, a JSON Schema and an example.

   An agent's writes are ordinary edits judged by the agent's own grant
   (docs/AGENTS_CONTRACT.md). A plugin's approval (what it may read and change) is about
   the plugin's code, which never runs here, so an agent needs none.
2. **The guide rides in the module, as a WebAssembly custom section `pimble-guide`.** The
   SDK's `plugin!` macro embeds a `guide.json` the plugin's crate holds. This means:
   - the MCP reads it from the `.wasm` files in the plugins folder without running or
     compiling anything, and with Pimble closed;
   - a plugin updated from the library brings its new guide with it;
   - the plugin ABI does not change: no new export, and the manifest is untouched.
3. **The MCP checks settings against the schema before writing**, and a refusal names the
   place and what was wanted ("year[3].days must be a whole number"). Nothing is written
   then. The schema is a small subset of JSON Schema:
   - `type`, `properties`, `required`, `additionalProperties`;
   - `items`, `enum`, `minimum`, `maximum`, `minItems`;
   - `anyOf`, `description`.

   The check is `pimble_plugin_abi::guide::check`, and each plugin's own tests run the
   same check on what its real types write, so the schema cannot drift from the code.
4. **Settings are written whole, as one edit** (`NodeDoc::set_data` of the viewer's key
   through `applyEdit`), as the calendar form writes a calendar it starts. A key the
   agent leaves out is gone. `get_viewer_settings` first is the way to change one thing.

## 3. The MCP

- **`list_viewers`** lists every installed viewer with its guide. It also covers the
  app's own `list` viewer (query metanodes, docs/QUERY_METANODES_CONTRACT.md) and says
  which plugins are installed without a guide.
- **`get_viewer_settings { node, viewer? }`** returns the node's `data.<viewer>` as JSON.
  The viewer defaults to the metanode's own; the answer is `{}` when nothing is set.
- **`set_viewer_settings { node, settings, viewer? }`** checks the settings against the
  viewer's schema, then writes them.
  - A viewer with no guide, or a guide with no settings, is refused with a sentence.
  - A node that is not a metanode shown with that viewer is refused, naming the viewer
    it is shown with.
- **`get_node` on a metanode** says where its guide is: "Its viewer's fields and settings:
  list_viewers; its settings now: get_viewer_settings."
- **`set_metanode` with a viewer nothing installed provides** still writes, as today: the
  app shows the outline until one is installed. The answer then names the viewers that
  are installed.
- **The server instructions** mention `list_viewers` in the paragraph on metanodes.

Where the guides are read: `<data dir>/pimble/plugins/*.wasm`, the folder the desktop app
loads, read again on every call (a few file reads; nothing cached that could go stale).

## 4. The guides as shipped

- **kanban:** the source is the metanode's children two levels deep. The first level is
  columns and the second is cards, in the tree's order (which is priority). A column that
  is itself a kanban metanode is a lane. No fields, no settings.
- **todo:** the source is the metanode's subtree; every node is an item, and an item's
  children are its sub-todos. Fields:
  - `done`: the moment it was checked, RFC 3339; absent while undone;
  - `due`: a date as a person types it.

  No settings.
- **timeline:**
  - Fields: `date` (one moment), or `start` and `end` (a span), written as the timeline's
    calendar writes dates. Rough dates read ("c. 1066", "early 1400s", "412-415 AF").
  - Settings:
    - `calendar`: a calendar node's pimble: link, or absent for the Gregorian;
    - `also`: the calendars shown beside it, each `"gregorian"` or a link;
    - `start`, `date` and `end`: other field names to read.
- **calendar:** a node of its own (a metanode shown with the `calendar` viewer). Its
  settings are the calendar's definition (docs/TIMELINE_CONTRACT.md section 3): name, day,
  week, year (months and days of no month), leap rule, epoch, eras, seasons and writes.
  The schema is the definition's shape, and the example is the invented calendar "New
  Calendar" starts from.

## 5. Waves

1. **The machinery:**
   - `pimble_plugin_abi::guide`: the types, reading the custom section, and the check;
   - the SDK macro's `guide:`;
   - the four guides, with tests;
   - the three MCP tools, the `get_node` line and the `set_metanode` note.
2. **A blind agent's trial.** A fresh agent with only the MCP, on a scratch server, is
   asked to make a timeline of the Roman Republic and a second one in an invented
   calendar of its own. Where it stumbles goes into the guides or the tools.
