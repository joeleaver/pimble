# What plugins tell agents: viewer guides

Status: waves 1 and 2 built 2026-11-06 on branch `agent-guides` (sections 6 and 7); wave 3,
plugin tools, decided by Joe and built the same day (sections 8 and 9); a second blind
trial and its fixes (section 10). Joe asked for it the same
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

## 8. Plugin tools (decided by Joe 2026-11-06; wave 3)

Joe chose the first way, and made it general: "This has to be a generic solution for many
types of plugins, not just what already exists." His answers:

1. **A plugin tool may read and write.** Every edit is an ordinary `applyEdit`, judged by
   the agent's grant and attributed to the agent.
2. **It is bounded by the agent's grant and by the plugin's own manifest asks.** The
   approval it runs under is built from what the manifest asks: stores `none`, `all`, or
   `chosen` (read as every store the agent's grant reaches), and `write` only when the
   manifest asks to write. There is no second approval prompt.
3. **A failed check refuses the write.** Nothing is written, and the answer carries the
   plugin's sentence. A check that passes adds what it said to the write's answer
   ("reads as Mar 15, 44 BC").

**The shape:**
- **Declared in the guide.** `tools` is a list of `{ name, about, params }`, where
  `params` is a JSON Schema checked with `guide::check` before the plugin runs.
- **Two names are conventions the MCP runs by itself:**
  - `check_settings { viewer, metanode, settings }`, before `set_viewer_settings`
    writes;
  - `check_field { viewer, metanode, node, field, value }`, before `set_fields` writes
    a field that a viewer showing the node names in its guide's `fields`.
- **Every other tool** is called by name with `call_plugin_tool`.
- **The answer** is `{ "ok": bool, "said": "..." }`.
- **Optional exports, so no ABI change:**
  - `pimble_tool(ptr, len) -> u64` starts the tool as a task in the plugin (the request
    is the JSON `{ tool, args }`) and answers its task id;
  - `pimble_tool_result(id) -> u64` answers the packed result bytes, or 0 while it runs.

  The SDK's `plugin!` takes `tools: <fn(ToolCall, Arc<Pimble>) -> ToolFuture>`. A plugin
  without the exports has no tools.
- **The headless runner** (`pimble_plugin_host::tools`, feature `wasm-host`) instantiates
  the module with no document. It drives the tool's task through the same exports a
  mounted view uses: `poll_task`, `fire_timeout`, `drain_polls` and `resolve`.
  - API calls go through `bridge::execute` with the approval above, through the MCP's
    own connection, so the server judges them by the agent's grant.
  - A tool that draws (`flush`) is refused.
  - Each export call keeps the 2 s budget, and a whole tool gets 30 s.
- **The MCP** keeps compiled plugins for as long as it runs, keyed by file and modified
  time, and compiles those with tools in the background when it starts.

**The timeline's tools** (wave 3):
- `check_field`: a date read in the timeline's calendar, answering "reads as Oct 14,
  1066" or why it does not read and the forms that do;
- `check_settings`:
  - for a timeline, that its calendar link names a calendar node;
  - for a calendar, that it reads and makes sense: seasons name months the year has, an
    era counting up has `from` and one counting down has `to`, and there is a quiet era;
- `read_date { calendar, text }`: what a text reads as in a calendar (a calendar node's
  link, or "gregorian"), checked in that calendar's own terms (section 11).

## 9. Wave 3 as built (2026-11-06)

- **ABI**, with no version change:
  - the optional exports `pimble_tool` and `pimble_tool_result` (`exports::TOOL`,
    `exports::TOOL_RESULT`);
  - in `guide`: `ToolGuide`, `ToolRequest`, `ToolAnswer`, `CHECK_SETTINGS` and
    `CHECK_FIELD`;
  - `Guide.tools`.
- **SDK:**
  - `plugin!(describe, view, guide, tools: <ToolFn>)`;
  - `wasm::tool` starts the tool as a `spawn_local` task, and `wasm::tool_result` hands
    its answer over once;
  - `ToolAnswer`, `ToolFuture` and `ToolRequest` in the prelude.
- **Host:**
  - `Call::Tool` and `Call::ToolResult` in both engines; the browser engine refuses them,
    since tools run for agents and never in a page;
  - `instance::register_host` and `forget_host`, for a host with no `Instance`;
  - `tools::run`, the headless driver;
  - `tools::agent_approval`, which builds the approval from the manifest's asks.
- **MCP:**
  - `call_plugin_tool` checks the arguments against the tool's `params`, then runs it;
  - `list_viewers` lists each plugin's tools and says which two run by themselves;
  - `set_viewer_settings` runs `check_settings` after the schema check;
  - `set_fields` runs `check_field` for every metanode showing the node (a parent within
    its source's depth) whose plugin has it. A refusal names the field, the node, the
    viewer and the metanode. A passing check is said only for the fields the viewer's
    guide lists.
  - Tools run on a blocking thread with a current-thread runtime and a `LocalSet`, over
    the agent's own connection.
  - Plugins are compiled once per file version, and those with tools are compiled in the
    background when the MCP starts.
- **The timeline** (`plugins/timeline-viewer/src/tools.rs`):
  - `check_settings` for both of its viewers: a calendar goes through `Calendar::from_json`
    (the calendar form's own `problem()`), and its seasons must name its months;
  - `check_field`, read with the viewer's own `read_said` in its calendar, then in the
    ones beside it;
  - `read_date`.

  A good calendar is described back: its days a year, how the absolute years 0 and -1
  are written, where year 0 begins in the Gregorian calendar, and how today reads.
- **hello-viewer** carries four test tools: an echo, a read, a write and a field check.

Checked:
- **`pimble-plugin-host` `tools`:** hello's tools answer, read (two children), write (a
  rename the server then holds), check (a bad mood refused, a fine one passed), and
  refuse an unknown tool in words.
- **`timeline_tools`,** on the built timeline:
  - "Mar 15, 44 BC" reads as itself;
  - "Blorpuary 99, banana" does not read, with forms that do;
  - a field it does not read passes;
  - the example calendar is described;
  - a season naming a missing month is refused;
  - a non-link and a node that is no calendar are refused;
  - a real calendar node is accepted with the Gregorian beside it;
  - "Fir 3, 1300 YF" and "Year's Gate, 12 BF" read (with their weekday and day of the
    year, and no Gregorian date since section 11), and "c. 1066" reads.
- **The MCP's end-to-end test,** with the real built plugin in the plugins folder, adds:
  - the tools listed;
  - a timeline's bad calendar link refused with the plugin's sentence, and a good one
    written with "It counts in the Founding calendar.";
  - "Blorpuary 99, banana" refused by `set_fields`, with nothing written;
  - "Fir 3, 1 YF" written and said back, with a field the viewer does not read left
    unmentioned;
  - `call_plugin_tool` reading "Year's Gate, 3 YF", and refusing missing arguments and
    an unknown tool.
- **Whole suites:** `pimble-mcp` 35 tests, `pimble-plugin-host` (every suite, with the
  built plugins) and the plugins' own tests all pass; the workspace and the browser build
  cleanly.

## 10. The second blind trial (2026-11-06)

A fresh agent got the same task, in a fresh store "Atlas" and a world "Veyra", with the
plugin tools in place. It tried every form with `read_date` before writing, wrote every
date through the checks, and checked the Gregorian date each answer gave against its own
sums. It made:
- a Roman Republic timeline of 14 events;
- "Veyran Reckoning": ten months of 36 or 37 days, three days of no month (one only in
  leap years), a six-day week, eras BV and AV, and year 0 on Mar 21, 1000;
- "Chronicle of Veyra": 10 events in three periods.

Opened in the app, the chronicle drew all 10, where `outline` said they would be.

What it found, and what came of it:
- **A real bug in the timeline's date reading, which no person had hit.**
  - "Dee 36, 1 BV" read as day 1 of 36 BV. The day-or-year guess allowed days only up
    to 31, whatever the month held.
  - "Sec 36, 1", in a 30-day month, read as day 1 of year 36 instead of not reading.

  `Calendar::read` now:
  - allows a day whatever the month holds in a leap year;
  - reads two numbers after the month's name in the calendar's order, the day then the
    year, so a day the month lacks does not read;
  - still guesses only when a number comes before the month ("14 Oct 1066", "1066 Oct
    14").

  Test: `a_day_is_whatever_its_month_holds_and_spans_count_down_too`.
- **"264-241 BC" did not read,** while the refusal suggested "-". The two-number
  shorthand now takes a falling pair, since years count down in BC, and a span that runs
  backwards on the line does not read.
- **A refusal named the parent** ("date on The Founding") instead of the node written.
  It now names the node.
- **The guides still said "nothing checks a date".** They now say:
  - what the checks do;
  - that `read_date` tries a form;
  - that the Gregorian calendar is proleptic, so dates before 1582 are not the Julian
    dates many histories give;
  - that an end runs through the end of what it names ("241 BC" through Dec 31, 241 BC).
- **The leap year was given as "such as 5"** without saying it was the written year. It
  now reads "such as the year written 5, absolute 4".
- **Ends:**
  - `check_field` on an end says what it runs through: "As an end it runs through Dec 31,
    44 BC.";
  - a start or an end that would put the end before the start is refused, as the line
    would draw no bar.
- **No way to see a timeline.** The timeline now has an `outline` tool: every node on the
  line in time order, with its first and last day, whether it is a point, a span or a bar,
  and its Gregorian days in an invented calendar when the timeline shows the Gregorian
  beside it (section 11); then the group labels, and what is not
  shown as meant. Run on the agent's own two timelines, it listed every event as the app
  then drew it.
- **Left as they are:**
  - a Gregorian date is accepted on a timeline that shows the Gregorian beside its own
    calendar, as documented, and the answer names the calendar it read in;
  - `check_field` through `call_plugin_tool` takes links, not paths, as its parameter
    says;
  - the trial client's missing `$defs` (section 7).

Checked: the timeline's 22 tests; `timeline_tools` with the end, an end before its start,
"264-241 BC" and `outline`; the MCP's end-to-end test.

## 11. Gregorian dates only where the timeline shows them (Joe, 2026-11-07)

Joe asked what a Gregorian date is for in a made-up calendar. It let an agent check the
calendar's arithmetic, but for an invented world it is noise, and it invites the agent to
reason in our dates instead of the world's. So:

- `check_field`, `read_date` and `outline` check a reading in the calendar's own terms: a
  day's weekday (when the calendar has weeks) and its day of the year ("a Thirdday, day 4
  of 181 in its year"); a year, a month, a rough date or a span its length in days.
- A Gregorian date is added only when the timeline shows the Gregorian beside its own
  calendar (`also` names it), so the agent sees what the person sees: "it begins on Jul
  10, 355 BC in the Gregorian calendar beside it", and outline's "[Gregorian A to B]".
  `read_date` names one calendar and never adds it.
- The calendar's description keeps where year 0 begins in the Gregorian calendar: the
  epoch is counted from the Gregorian 1 January AD 1, so that is the anchor the agent set.
- Tests: the timeline plugin's unit test reads an invented date alone (no Gregorian) and
  beside the Gregorian (named); the host test's answers carry the weekday and day of the
  year.
