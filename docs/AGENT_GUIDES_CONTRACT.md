# What plugins tell agents: viewer guides

Status: waves 1 and 2 built 2026-11-06 on branch `agent-guides` (sections 6 and 7); one
open question for Joe (section 8). Joe asked for it the same
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

## 6. Wave 1 as built (2026-11-06)

As sections 2 to 4 say:
- `pimble_plugin_abi::guide` holds the types, `read`, `custom_section` and `check`;
- `plugin!`'s `guide:` arm adds a `#[used]` static in the `pimble-guide` link section;
- `crates/pimble-mcp/src/tools/viewer_tools.rs` holds the three tools.

Checked:
- **`guide` tests (3).** A refusal names the place: "year[0] must be one of: a month...",
  "the settings object has no colour; it takes ...". An example that does not fit its
  own schema fails the guide. The section is read out of a module.
- **Each plugin's `the_guide_reads_and_names_its_viewers`,** and the timeline's
  `the_calendar_schema_fits_the_calendars_the_code_writes`:
  - the Gregorian and invented calendars, as serde writes them, fit the schema;
  - the example reads as a `Calendar`;
  - the timeline's example names a calendar.
- **The MCP's `an_agent_reads_a_guide_and_writes_settings_that_fit_it`,** on a real
  server with a module carrying the timeline's own `guide.json`:
  - listing the viewers;
  - a node that is not a metanode is refused;
  - a misshapen calendar is refused and nothing is written;
  - a good one is written and read back;
  - another viewer's settings are refused;
  - `get_node`'s pointer, and `set_metanode`'s note for a viewer nothing provides.
- **The built modules** carry the section: `pimble-guide`, `name`, `producers`,
  `target_features`.

## 7. Wave 2: the blind trial (2026-11-06)

A fresh agent got only a stdio MCP client (the server's instructions and the tools'
descriptions, as a real client shows them) and an empty scratch store. Its task: a Roman
Republic timeline, then a calendar for an invented world and a timeline counting in it. It
made three things without a failed call:
- **"Roman Republic":** 16 events in three groups, BC days and spans;
- **"Reckoning of the Kindling":** eight months of 45 or 46 days, three days of no month
  (one only in leap years), a six-day week, a leap rule with an exception, four seasons,
  eras BK and AK, and year 0 at 1 January 1200;
- **"Chronicle of Aerth":** 9 events in it, with the Gregorian shown beside.

**Opened in the app afterwards, both drew right.** Every event of both was on the line:
BC spans as bars, "Yearsdawn, 1 AK" and "Midsummer Feast, 310 AK" as points, "spring of
40 BK" as a soft span, and the Gregorian axis lined up with 1 AK at 1200.

What it stumbled on, and what came of it:
- **Days of no month, eras and absolute years, and the epoch** were guessed and worked
  out. The calendar's guide now gives:
  - the form "Year's Gate, 1300 YF";
  - which absolute year is which era's year 1;
  - what the leap rule tests;
  - the epoch as whole days times 86400, with a worked example.
- **Refusals read "the settings has no ..."**: now "the settings object".
- **Nothing checks a date, a calendar link or a calendar's meaning when written.**
  "Blorpuary 99, banana" was taken as a date, "not a link" as a calendar, and a season
  naming a month the year lacks as a season. The guides now say so (copy the forms shown;
  only the shape is checked). Closing it is section 8.
- **A viewer made with `set_metanode` keeps the plain node icon**, where New Timeline
  gives its recipe's icon. The MCP sets no icons. Left as it is.
- **`$defs` referenced by tool schemas** were missing from the trial client's listing.
  That is the client's printing, not the server's; real MCP clients resolve them.

## 8. Open: checking what only the plugin understands (Joe's call)

How a date reads, whether a calendar link names a calendar, and whether a calendar makes
sense are known only to the plugin's own code. Decision 1 keeps plugin code out of the
agent's path, and the timeline contract's decision 4 keeps calendars inside the plugin.
So the MCP can check only the shape. The ways to close it:

1. **A pure check the plugin exports**, run by the MCP under wasmtime: settings or a
   field value in, sentences out. Nothing is mounted, and the API is not reachable from
   it. It needs wasmtime in `pimble-mcp`, which is the size of the app's plugin host, and
   about a second to compile the timeline the first time (cacheable).
2. **Leave it**, as now: the guides teach the forms, and the trial showed an agent
   following them gets it right. A wrong date shows to the person as an event missing
   from the line.
