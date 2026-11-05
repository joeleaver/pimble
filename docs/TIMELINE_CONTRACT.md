# Timeline contract: nodes on a line of time, in any calendar

Status: proposed 2026-11-05, not built; Joe's answers of 2026-11-05 are in. Comes after
the todo list (built, docs/TODO_CONTRACT.md). One question is open: what a calendar looks
like (section 3, a proposal and its examples for Joe to judge).

A timeline is a metanode shown with the `timeline` viewer of a `timeline-viewer` plugin:
its source's nodes placed on a horizontal line of time by their own node fields (the
key/value store the Properties panel shows), a point for a node with one moment and a bar
for one with a start and an end. It opens things as the kanban does (beside, keeping the
focus) and looks like the kanban for now (`pimble_ui::viewer`).

## 1. Decisions (Joe, 2026-11-05)

1. **Any calendar, real or invented.** History as well as a fantasy or science-fiction
   novel's planning: the Gregorian calendar with BC/AD, and calendars a writer makes up:
   their own months and their lengths, weeks, eras and epochs ("Third Age 3019", "412
   AF"), years of any length, or no months at all.
2. **Hours, minutes and seconds in every calendar, real or imagined**: a calendar says how
   long its day is and how it divides (a 26-hour day of 100-minute hours is a calendar).
3. **Fuzzy dates** ("c. 300 BC", "early 1400s", "spring of 412"): yes, drawn as soft
   ranges (a later wave).
4. **Timeline stuff stays inside the plugin.** The calendars, the reading and writing of
   dates in them, and everything else of the timeline's live in `timeline-viewer`; sheets
   keep their own Gregorian dates, and nothing else in Pimble changes for it.
5. Which fields a timeline reads is its own setting; the defaults are `start` and `end`,
   falling back to `date`. Opening and the look are the kanban's.

## 2. The line

Every calendar places a moment on one line: whole **seconds from a fixed origin**, a
signed 64-bit count (±292 billion years, so deep time and far futures fit). The origin is
the Gregorian calendar's 1 January AD 1, 00:00:00; an invented calendar says where its
first era starts on that line, which is also how two calendars are lined up on one
timeline (wave 4). A calendar's own numbers (its day of 26 hours, its year of 412 days)
are only how it reads and writes points on the line.

## 3. What a calendar looks like (proposed, for Joe)

A **calendar node**: a node of its own in the tree (type `document`, shown with the
plugin's `calendar` viewer), its definition in the node's `data` under `calendar`, so it is
co-edited like anything else, travels with its store, and is named by a timeline's
settings as a pimble: link. The Gregorian calendar is built into the plugin and is the
default; a writer's calendar starts as a copy of a recipe ("New Calendar") and is edited
in the calendar viewer, a form (names, lengths, eras) with a live sample ("Today in this
calendar reads ..."), never raw JSON. The definition, shown here as JSON:

```json
{
  "name": "Shire Reckoning",
  "day": { "hours": 24, "minutes": 60, "seconds": 60 },
  "week": ["Sterday", "Sunday", "Monday", "Trewsday", "Hevensday", "Mersday", "Highday"],
  "year": [
    { "day": "2 Yule" },
    { "month": "Afteryule", "short": "Aft", "days": 30 },
    { "month": "Solmath", "short": "Sol", "days": 30 },
    { "month": "Rethe", "short": "Ret", "days": 30 },
    { "month": "Astron", "short": "Ast", "days": 30 },
    { "month": "Thrimidge", "short": "Thri", "days": 30 },
    { "month": "Forelithe", "short": "Fore", "days": 30 },
    { "day": "1 Lithe" },
    { "day": "Mid-year's Day" },
    { "day": "Overlithe", "leap": true },
    { "day": "2 Lithe" },
    { "month": "Afterlithe", "short": "Afterl", "days": 30 },
    { "month": "Wedmath", "short": "Wed", "days": 30 },
    { "month": "Halimath", "short": "Hali", "days": 30 },
    { "month": "Winterfilth", "short": "Wint", "days": 30 },
    { "month": "Blotmath", "short": "Blot", "days": 30 },
    { "month": "Foreyule", "short": "Forey", "days": 30 },
    { "day": "1 Yule" }
  ],
  "leap": { "every": 4, "except_every": 100 },
  "eras": [
    { "name": "Third Age", "short": "T.A.", "starts_at": -1300000000000, "counts": "up" },
    { "name": "Fourth Age", "short": "F.A.", "starts_after": { "era": "T.A.", "year": 3021, "month": "Halimath", "day": 29 }, "counts": "up" }
  ],
  "writes": "{day} {month} {year} {era}"
}
```

- **`day`**: how long a day is and how it divides. Every calendar has hours, minutes and
  seconds (decision 2); a day of `{ "hours": 26, "minutes": 100, "seconds": 100 }` is
  allowed.

  **An invented second is a real second** (Joe, 2026-11-05): the line's second, so a
  day of 26 hours really is longer than ours, and a timeline showing two calendars lines
  their moments up truthfully.
- **`week`**: day names, optional (no week, no weekday names).
- **`year`**: the year in order: months (a name, a short name, a length in days) and days
  that belong to no month (Yule, Lithe, a festival week); `leap: true` marks a day or a
  month that is there only in leap years. A calendar with no months is one entry,
  `{ "month": "Day", "days": 412 }`, written as "Day 214, 3088 CE".
- **`leap`**: one rule from a small fixed set: none; every `n` years; every `n` except
  every `m`, except every `k` (the Gregorian rule is `4 / 100 / 400`).
- **`eras`**: each with a name and short name; where it starts on the line (`starts_at`,
  seconds) or right after a moment of the era before (`starts_after`); and whether years
  count up or down in it. BC is an era counting down that ends where AD begins, with no
  year zero (the Gregorian calendar is exactly that, described this way: nothing about it
  is special in the code). An era may also end (`ends_after`) with no era following.
- **`writes`**: how a moment is written, from parts (`{day}`, `{month}`, `{short}`,
  `{year}`, `{era}`, `{weekday}`, `{hh}:{mm}:{ss}`). Reading accepts that order and the
  common variations (short names, any case, the era before or after the year, a time
  after the date).

Two more, to show the range:

```json
{ "name": "Gregorian", "day": { "hours": 24, "minutes": 60, "seconds": 60 },
  "week": ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
  "year": [{ "month": "January", "short": "Jan", "days": 31 },
           { "month": "February", "short": "Feb", "days": 28 },
           { "month": "Leap day", "days": 1, "leap": true, "joins": "February" },
           "... the other ten months ..."],
  "leap": { "every": 4, "except_every": 100, "except_except_every": 400 },
  "eras": [{ "name": "Before Christ", "short": "BC", "ends_at": 0, "counts": "down" },
           { "name": "Anno Domini", "short": "AD", "starts_at": 0, "counts": "up" }],
  "writes": "{short} {day}, {year} {era}" }

{ "name": "Kepler Colony Standard", "day": { "hours": 26, "minutes": 60, "seconds": 60 },
  "year": [{ "month": "Day", "days": 412 }],
  "leap": { "none": true },
  "eras": [{ "name": "After Founding", "short": "AF", "starts_at": 69400000000, "counts": "up" }],
  "writes": "Day {day}, {year} {era}" }
```

(The `starts_at` numbers here are made up for the examples. `joins` puts a leap day at the end of the month it names, so 29 February is a day of
February, not a month of its own.)

**Fuzzy dates** (decision 3, a later wave): a field reads as a soft range when it says
so: "c. 412 AF" (about a year either side), "early 1400s" (the first third of the
century), "spring of 412" (a named part of the year, which a calendar may define as
`seasons`), "412-415 AF". Drawn with faded ends; sorted by their middle.

## 4. What the person sees (proposed)

- A horizontal axis in the timeline's calendar, labeled at the zoom: seconds and minutes
  up through days, months, years, decades, centuries, ages. Zoom with Ctrl+wheel and
  pinch; scroll sideways.
- Rows: with a two-level source, the first level becomes swimlanes (as kanban lanes);
  otherwise one row per node, packed so bars do not overlap.
- Fields that do not read as a moment in the timeline's calendar are listed beside the
  line, as typed, never guessed at.
- Dragging a bar moves its moments, dragging an edge changes the start or end: one field
  edit through the API each, written back in the calendar's `writes`. A double-click on
  an empty spot makes a node at that moment.
- Click opens beside; Ctrl+click in place; double-click on a bar or F2 renames.
- Zoom, scroll position and folded lanes are this device's own (`Host::remember`).

## 5. Waves (proposed)

1. The line, the Gregorian calendar (BC/AD, hours, minutes, seconds) built into the
   plugin; the timeline over it: points and bars, rows, zoom and scroll, opening.
2. Calendar nodes: the definition above, the calendar viewer's form, "New Calendar"; a
   timeline naming its calendar.
3. Dragging moments, making nodes on the line.
4. Several calendars on one timeline, lined up; fuzzy dates.
