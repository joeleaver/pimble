# Timeline contract: nodes on a line of time, in any calendar

Status: all four waves built 2026-11-05 (branch `timeline`): the line, the Gregorian
calendar defined as data, points and bars, lanes, zoom and scroll, opening (section 6);
dragging moments and making nodes on the line (section 7); calendar nodes made up in a
form, and a timeline choosing its calendar and its fields (section 8); several calendars
on one timeline, rough dates, presence on bars, how a calendar writes, its seasons and the
week's first day (section 9). Joe said "keep building" without judging section 3, so it
was built as proposed; it is still his to change.

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

A **calendar node**: a node of its own in the tree (a metanode shown with the plugin's
`calendar` viewer), its definition in the node's `data` under `calendar` (the viewer's own
settings), so it is
co-edited like anything else, travels with its store, and is named by a timeline's
settings as a pimble: link. The Gregorian calendar is built into the plugin and is the
default; a writer's calendar starts as a copy of a recipe ("New Calendar") and is edited
in the calendar viewer, a form (names, lengths, eras) with a live sample ("Today in this
calendar reads ..."), never raw JSON. The definition, shown here as JSON (`timeline-viewer/src/calendar.rs`, `Calendar`, is
this shape; wave 1 uses only its Gregorian instance):

```json
{
  "name": "Shire Reckoning",
  "day": { "hours": 24, "minutes": 60, "seconds": 60 },
  "week": ["Sterday", "Sunday", "Monday", "Trewsday", "Hevensday", "Mersday", "Highday"],
  "year": [
    { "day": "2 Yule" },
    { "month": "Afteryule", "short": "Aft", "days": 30 },
    { "month": "Solmath", "short": "Sol", "days": 30 },
    "... four more months ...",
    { "day": "1 Lithe" },
    { "day": "Mid-year's Day" },
    { "day": "Overlithe", "leap": true },
    { "day": "2 Lithe" },
    { "month": "Afterlithe", "short": "Afterl", "days": 30 },
    "... five more months ...",
    { "day": "1 Yule" }
  ],
  "leap": { "every": 4, "except_every": 100 },
  "epoch": -1300000000000,
  "eras": [
    { "name": "Third Age", "short": "T.A.", "from": 0, "to": 3020, "counts": "up" },
    { "name": "Fourth Age", "short": "F.A.", "from": 3021, "counts": "up", "quiet": true }
  ]
}
```

- **`day`**: how long a day is and how it divides. Every calendar has hours, minutes and
  seconds (decision 2); a day of `{ "hours": 26, "minutes": 100, "seconds": 100 }` is
  allowed.

  **An invented second is a real second** (Joe, 2026-11-05): the line's second, so a
  day of 26 hours really is longer than ours, and a timeline showing two calendars lines
  their moments up truthfully.
- **`week`**: day names, optional; `week_starts` says which of them the first day of the
  calendar's year 0 is (the Gregorian 1 January AD 1 was a Monday, so 0 there).
- **`year`**: the year in order: months (a name, a short name, a length in days, and
  `leap_days` it gains in a leap year: February's one) and days that belong to no month
  (Yule, Lithe, a festival), `leap: true` when such a day is there only in leap years. A
  calendar with no months is one entry, `{ "month": "Day", "days": 412 }`, written as
  "Day 214, 3088 AF".
- **`leap`**: one rule from a small fixed set: none; every `n` years; every `n` except
  every `m`, except every `k` (the Gregorian rule is `4 / 100 / 400`), tested on the year
  plus `offset` (the Gregorian rule is tested on the astronomical year, so 1 BC is leap).
- **`epoch`**: where the calendar's year 0 begins on the line, in seconds. Years are
  numbered absolutely from it (any integer); **`eras`** say how a person numbers them:
  counting up from an era's first year (`from`) or down to its last (`to`), with no year
  zero, `also` for other ways of writing it ("BCE", "B.C."), and `quiet` for the era left
  out when writing and assumed when a year is typed without one (AD). BC is an era
  counting down to absolute year -1, AD one counting up from 0: the Gregorian calendar is
  exactly that, described this way, and nothing about it is special in the code.
- **Reading** takes "1066", "44 BC", "Oct 1066", "Oct 14, 1066", "14 October 1066",
  "1066-10-14" (ISO's astronomical years: -0043 is 44 BC), "10/14/1066", a time after
  the date ("14:30", "2:30 pm", "14:30:05"), and a day of no month by its name
  ("Mid-year's Day 1420"). A year is needed and nothing is guessed: "Oct 14" alone does
  not read (a day with no year, or October of AD 14). What was given fixes the
  precision (a year, a month, a day, a minute, a second), and the span a moment names
  ends where the next unit begins. **Writing** is "Oct 14, 1066", "44 BC", "Oct 14, 1066
  14:30"; **`writes`** changes that within a small fixed set, all of which read back:
  `day_first` ("14 Oct 1066"), `full_months` ("October"), `weekday` ("Saturday, ...";
  a day name typed before a date is passed over when reading).
- **`seasons`**: named runs of months (`name`, `also`, `from`, `to`, months by name),
  into the next year when `to` comes before `from`: the Gregorian has Spring (March to
  May), Summer, Autumn (also Fall) and Winter (December to February), so "winter of
  1066" ends with February 1067.

A colony calendar, to show the range (its definition round-trips in the plugin's tests):

```json
{ "name": "Kepler", "day": { "hours": 26, "minutes": 60, "seconds": 60 },
  "year": [{ "month": "Day", "days": 412 }, { "day": "Founding Day" }],
  "epoch": 86400000000,
  "eras": [{ "name": "After Founding", "short": "AF", "from": 0, "counts": "up" }] }
```

(The `epoch` numbers here are made up for the examples.)

**Fuzzy dates** (decision 3, wave 4, section 9): a field reads as a soft range when it
says so: "c. 412 AF" (about: a unit of its own either side, and for a year more the
rounder it is: "c. 1066" a year, "c. 1060" five, "c. 300 BC" fifty), "early 1400s"
(early, mid and late are thirds of what follows them), "the 1960s", "the 15th century
BC", "spring of 412" (a season of the calendar's). Two moments in one field ("412-415
AF", "1939 to 1945", "44 BC – AD 14") are a span with firm ends. Rough ones are drawn
with faded ends.

## 4. What the person sees (proposed)

- A horizontal axis in the timeline's calendar, labeled at the zoom: seconds and minutes
  up through days, months, years, decades, centuries, ages. Zoom with Ctrl+wheel and
  pinch; scroll sideways.
- Rows: with a two-level source, the first level becomes swimlanes (as kanban lanes);
  otherwise one row per node, packed so bars do not overlap.
- Fields that do not read as a moment in the timeline's calendar are listed beside the
  line, as typed, never guessed at.
- Dragging a bar moves its moments, dragging an edge changes the start or end: one field
  edit through the API each, written back as the calendar writes (section 7). A
  double-click on an empty spot makes a node at that moment.
- Click opens beside; Ctrl+click in place; double-click on a bar or F2 renames.
- Zoom, scroll position and folded lanes are this device's own (`Host::remember`).

## 5. Waves

1. The line, the Gregorian calendar (BC/AD, hours, minutes, seconds) built into the
   plugin; the timeline over it: points and bars, rows, zoom and scroll, opening. Built.
2. Calendar nodes: the definition above, the calendar viewer's form, "New Calendar"; a
   timeline naming its calendar. Built (section 8).
3. Dragging moments, making nodes on the line. Built (section 7).
4. Several calendars on one timeline, lined up; fuzzy dates. Built (section 9).

## 6. Wave 1 as built (2026-11-05)

- `timeline-viewer` (`plugins/timeline-viewer`): `calendar.rs` (the calendar as data,
  the line, reading and writing), `layout.rs` (placing, lanes, packing, ticks), `lib.rs`
  (the view). "New Timeline" makes a metanode two levels deep; the viewer takes any source.
- **On the line**: a node's `start` (else `date`) and `end`. Two moments are a bar from
  the start to where the end's unit ends; one moment given to a day or finer is a point,
  one given only to a month or a year is that month's or year's span. A top node with no
  moment of its own whose subtree has one is a lane (its name over its band); the rest
  share the unnamed band at the top. Rows are packed first-fit by start so no two bars
  or labels overlap, again at every zoom. Nodes whose fields do not read are listed
  below the line: "Its start “whenever” is not a date in the Gregorian calendar."; "It
  ends before it starts."
- **The axis** picks the smallest step whose ticks are at least 84 px apart: 1, 5, 15, 30
  seconds or minutes; 1, 3, 6, 12 hours; 1, 2, 7 (Mondays), 14 days; 1, 3, 6 months;
  1, 2, 5 times powers of ten years, on round numbers in each era with no year 0, an
  era's first year marked too (AD 1). Above the ticks the larger unit they fall in (the
  day over hours, the month and year over days, the year over months), the first held
  at the left edge. A line down from every tick; a red line at now.
- **Zoom** from a second across 2000 px to 30 billion years across 1000: Ctrl+wheel or a
  pinch around the pointer, + and - around the chosen node when it is in view; **pan**
  with a sideways swipe, Shift+wheel, a drag of the background, or the arrow keys
  (Shift for more); 0 shows everything, T goes to now; icons for each in the corner.
  The view is fitted to what is on the line at first, and remembered on this device
  (`Host::remember`, after a pause) from then on.
- **Opening** as the kanban: a click opens beside, Ctrl+click in place, a double-click or
  F2 renames, Delete deletes; Up and Down choose node after node in time, bringing each
  into view; Enter opens beside, Ctrl+Enter in place.
- **Checked**: the plugin's unit tests (the line meets Unix time at 1970, the Gregorian
  leap rule and 1 BC, BC counting down with no year 0, reading and writing to each
  precision and refusing what is not a moment, spans ending where the next unit begins,
  an invented 26-hour-day calendar on the same code, ticks from seconds to millions of
  years, zoom keeping the moment under the pointer, packing); pimble-plugin-host's
  `timeline` test over a real server (a span, a point in 1066, a lane, a node not on the
  line, open beside, a zoom remembered, a double-click rename); and by hand in the
  release app on a store copy (Rome in 44 BC and the Norman Conquest in lanes, fitted,
  then zoomed to the weeks around Hastings).
- **Not yet** (after wave 1): presence on bars, fuzzy dates, several calendars.

## 7. Wave 3 as built (2026-11-05)

- **A drag keeps what was typed.** Every moment keeps the precision it was given and lands
  on the start of its unit nearest where it was dragged (`Calendar::round`): a year moves
  by years, a day by days, a time typed to the minute by minutes, across BC and AD with no
  year 0. Moving a bar moves both ends by what its start moved, so it keeps its length;
  its ends (a grip on each, shown when the bar is at least 24 px wide, so a thin bar can
  still be grabbed whole) change only the start or only the end. Dragging an end of a
  one-moment span (a `date` of "Oct 1066") splits it into a start and an `end`, each the
  unit it was. A point has no ends. `layout::drag` is the one judgement of a drag: where
  the item is drawn while dragged, and the `(field, text)` pairs written, each into the
  field it was read from (`start` or `date`, `end`), as the calendar writes it.
- **While dragging** the item is lifted and drawn where it would land, and the status line
  says when that is ("1294 to 1295"), or "It would end before it starts." when it would
  (then nothing is written). Letting go writes every moment in one `edit` of the node;
  the item stays where it went until the change comes back. Leaving the panel puts it
  back. A press moves 3 px before it is a drag; a press let go where it was is the click
  (the desktop clicks on the press, a browser after the release: the click waits for the
  release either way, and the click that ends a drag opens nothing). Only writable
  nodes drag; the dragged node is the chosen one.
- **Alt+Left and Alt+Right** move the chosen node a step of its own unit (a day, a year)
  earlier or later, the same judgement as a drag.
- **A double-click on an empty spot** makes a node at that moment, in the lane clicked
  (else under the timeline itself), its `date` given as closely as the zoom shows
  (`layout::precision_at`: to the minute when a day is 600 px or wider, to the day when a
  day is 6 px, then the month, then the year), and its name is typed in place. A drag of
  the background is not a click.
- **Checked**: unit tests (`a_drag_moves_moments_by_their_own_units_and_writes_them_as_typed`,
  `rounding_lands_on_the_nearest_start_of_a_unit`); pimble-plugin-host's `timeline` test
  (a bar dragged in the desktop's order of events, its length kept and nothing opened; a
  press let go in place opening beside on the release; Alt+Right; a node made by a
  double-click on the line and named); and by hand in the release app (Domesday Book and
  the Battle of Hastings dragged, a node made by a double-click and named).
- **Not yet**: dragging a node into another lane (that is a move in the tree), snapping
  to other items, undo of a drag (the tree's undo, wave 4 there, is the one to come).

## 8. Wave 2 as built (2026-11-05)

- **A calendar node** is a metanode shown with the plugin's `calendar` viewer; its
  definition (section 3's shape, `calendar::Calendar`) is the viewer's settings,
  `data.calendar`. "New Calendar" makes one from the New menu; a new one offers "Start
  from the Gregorian calendar" and "Start an invented calendar" (twelve months of 30
  days, a 24-hour day, one era counting up from year 1).
- **The form** (`calendar_view.rs`) shows what is stored, finished or not: the name; the
  day's hours, minutes and seconds; the year in order (a month's name, short name, days
  and days more in leap years; a day of no month and whether it is there only in leap
  years), each row moved up or removed, "+ Month", "+ Day of no month"; the week's day
  names; the leap rule ("Every 4 years, except every 100, except every 400 after all;
  counted from year 1"); where year 0 begins, typed as a Gregorian date (that is how two
  calendars line up); the eras (name, short name, counting up or down, first and last
  year, assumed and left out or always written, other spellings), "+ Era". Below it, a
  live sample: "Today reads ..." and any date tried in it, with where it falls in the
  Gregorian calendar, or the sentence that says why the calendar cannot be used yet
  (`Calendar::problem`: "A day needs between 1 and 10,000 hours.", "Every month needs a
  name.", an era counting down without its last year, ...). Inputs commit on Enter or
  leaving them; a number that is not one is said and not written. A person who may only
  read sees the form disabled.
- **Each input writes only its own place**: `NodeDoc::edit_data` (pimble-crdt, a general
  mechanism of the `data` root, not the timeline's) sets, inserts or removes one key or
  array item by a path, so two people making up the same calendar each keep what they
  wrote; moving a row up is a remove and an insert in one edit.
- **A timeline's settings** are `data.timeline`: `calendar` (a pimble: link to a calendar
  node; none means the Gregorian calendar) and `start`, `end`, `date` (the field names it
  reads, defaulting to those). The gear in the timeline's bar opens them: a chip per
  calendar node of the store (the store walked when the strip opens) beside "Gregorian",
  "+ New calendar" and "+ Copy of the Gregorian" (each makes a calendar node beside the
  timeline, sets the timeline to it and opens it beside), and the three field names. The
  settings are shared by everyone who sees the timeline.
- **Reading in a calendar node**: the timeline reads, places, drags and writes in the
  calendar its settings name, and follows that calendar's edits as they happen (an era
  renamed rewrites every label; a date that no longer reads goes below the line, "Its
  start “Day 214, 3088 AF” is not a date in the Kepler calendar."). A calendar that
  cannot be used yet, or is gone, says so above the line ("“Kepler” can't be used yet: A
  day needs between 1 and 10,000 hours. Showing the Gregorian calendar meanwhile.") and
  the Gregorian calendar is used until it can.
- **Checked**: pimble-crdt's `edits_inside_data_touch_only_the_place_they_name_and_merge`
  (two documents changing different places merge to both; an insert and a remove at once;
  setting what is there writes nothing; a path that names nothing is refused with a
  sentence); the plugin's unit tests (`a_calendar_says_what_keeps_it_from_being_used`, the
  form reading what is stored, the sample); pimble-plugin-host's `timeline_calendar` tests
  (a calendar started from the template, a month renamed and an era's short name changed,
  each one place, a number refused, a date tried; a timeline set to a 26-hour,
  412-day calendar by a link and to its own field name, placing a node by a date in it,
  following an era renamed, going below the line when a spelling is taken away, and saying
  why a broken calendar is not used); and by hand in the release app (the settings
  strip, "+ New calendar" making one beside the timeline, the form and its sample).
- **Not yet** (after wave 2): a calendar's month or era can only be moved up, not
  dragged. The week's first day, how a calendar writes and calendars of other stores came
  with wave 4 (section 9).

## 9. Wave 4 as built (2026-11-05)

- **Several calendars on one timeline**: `data.timeline.also` lists other calendars
  (`"gregorian"` or a calendar node's pimble: link) shown under the timeline's own, each
  an axis of its own (its larger units over its ticks, its name at the right) at the same
  moments on the line, which is how they line up (each calendar's `epoch`). A field that
  does not read in the timeline's own calendar is read in them, in order; the node is
  placed, dragged and written back in the calendar it read in, and its hover says where
  that is in the timeline's own ("Day 214, 3088 AF, which is ... in the Gregorian
  calendar"). One that reads in none is listed below the line naming them all ("... is
  not a date in the Gregorian calendar or the Kepler calendar."). The settings strip's
  "Also show:" row has a chip per calendar other than the timeline's own; edits of any of
  them are followed as they happen, and one that cannot be used is named above the line
  and not shown.
- **Calendars of other stores**: the strip offers every calendar node of every store open
  here, the timeline's own store first, the others named "Kepler (Other store)".
- **Rough dates** (`Calendar::read_said`, section 3 "Fuzzy dates"): an "about" moment,
  two moments in one field, or a rough part of time. "About" and rough ones are drawn as a
  band of the accent color fading at both ends, dashed above and below. A drag keeps
  "c." ("c. 1066" moved four years writes "c. 1070") and writes two moments in one field
  back into it ("1939–1945"; a year range of one era is written that way, others "A to
  B"); a rough date is not moved ("“early 1400s” is a rough date: change it in its field
  to move it."). A field that is a span of its own beside another field naming its start
  or end is listed below the line saying so.
- **The calendar form** gains: "The first day of 1 YF is a" with a chip per day name
  (`week_starts`); "Writing a date" with today written each way (month first or day
  first), whole or short month names, with or without the day of the week; "Seasons" (a
  row each: name, from the start of a month, to the end of a month, other names; "+
  Season" starts at the first month), with a sentence when one names a month the year
  does not have. A calendar made before a part existed (no `writes`, no `seasons`) gets
  that part, empty, from the first input that writes into it, in the same edit.
- **Presence on bars**: who else is on a node shows on its bar or point with the app's own
  chips (`pimble_ui::presence::PersonChip`, three and "+n"), a click on one follows them,
  and a pane following someone brings the node they are on into view (docs/PRESENCE_CONTRACT.md
  "Plugins").
- **Checked**: the plugin's unit tests (`rough_dates_read_as_spans_and_ranges_in_one_field_as_two_moments`,
  `a_calendar_writes_days_its_own_way_and_reads_them_back`,
  `rough_and_about_dates_are_drawn_soft_and_drag_as_written`,
  `a_date_that_does_not_read_in_the_timelines_calendar_reads_in_another_it_shows`);
  pimble-plugin-host's `timeline_wave4` (Kepler's axis under the Gregorian one and a node
  read in it, a soft bar for "c. 2400", a rough date's hover, the axis and the reading
  gone when it is no longer shown, a person's chip on a bar and a click following them;
  the form writing the week's first day, day first and the weekday into a calendar made
  without `writes`, and a season into one made without `seasons`, then naming a month
  that is not there); and by hand in the release app (the Gregorian with an invented
  calendar under it, a node read in it, "early 1400s" fading, the form's new sections and
  its sample following them, an agent's chips on the bars it had just edited).
- **Not yet**: a calendar's month or era can only be moved up, not dragged; undo of a
  drag waits for the tree's undo; the plugin is checked on the desktop, not yet by hand in
  the browser.
