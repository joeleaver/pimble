//! Calendars as data (docs/TIMELINE_CONTRACT.md section 3): how a calendar's
//! days, years and eras sit on the line, and how a moment is read from what a
//! person typed and written back.
//!
//! **The line** is whole seconds from the start of the Gregorian 1 January
//! AD 1, a signed 64-bit count. A calendar places its year 0 at `epoch` on the
//! line and counts real seconds (an invented second is a real second, Joe,
//! 2026-11-05): a day of 26 hours is longer than ours.
//!
//! **Years** are numbered absolutely (`Y`, any integer, year 0 starting at
//! `epoch`); an era says how a person numbers them: counting up from its first
//! year, or down to its last (BC), with no year zero. The Gregorian calendar
//! is one definition of this shape ([`Calendar::gregorian`]): nothing about it
//! is special in the code.

use serde::{Deserialize, Serialize};

/// Seconds from 1 January AD 1 (Gregorian, proleptic) to 1 January 1970.
pub const UNIX_EPOCH_ON_LINE: i64 = 62_135_596_800;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    pub name: String,
    pub day: DayShape,
    /// Day names in order; empty for a calendar without weeks.
    #[serde(default)]
    pub week: Vec<String>,
    /// Which of `week` the first day of year 0 is (the Gregorian 1 January
    /// AD 1 was a Monday).
    #[serde(default)]
    pub week_starts: usize,
    /// The year in order: months, and days that belong to no month.
    pub year: Vec<Entry>,
    #[serde(default)]
    pub leap: Leap,
    /// Where this calendar's year 0 begins on the line, in seconds.
    #[serde(default)]
    pub epoch: i64,
    pub eras: Vec<Era>,
    /// Named parts of the year ("spring of 412"), each a run of months.
    #[serde(default)]
    pub seasons: Vec<Season>,
    /// How a date is written.
    #[serde(default)]
    pub writes: Writes,
}

/// A named part of the year: from the start of month `from` to the end of
/// month `to` (named as the year names them), into the next year when `to`
/// comes before `from` (winter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Season {
    pub name: String,
    #[serde(default)]
    pub also: Vec<String>,
    pub from: String,
    pub to: String,
}

/// How a calendar writes a day: "Oct 14, 1066" (the default) or "14 Oct
/// 1066", months short or whole, the day of the week before it or not. All
/// of them read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Writes {
    #[serde(default)]
    pub day_first: bool,
    #[serde(default)]
    pub full_months: bool,
    #[serde(default)]
    pub weekday: bool,
}

/// What a field says, read in a calendar (docs/TIMELINE_CONTRACT.md
/// "Fuzzy dates"): one moment, two written in one field ("412-415 AF"),
/// either "about" ("c. 1066"), or a rough part of time that no one moment
/// names ("early 1400s", "spring of 412", "the 15th century").
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    At { moment: Moment, about: bool },
    Between { from: Moment, to: Moment, about: bool },
    /// `from..to` on the line; `soft` when its ends are only roughly there
    /// (early, mid, late, about).
    Rough { from: i64, to: i64, soft: bool },
}

/// Ways of saying "about": "c. 1066", "circa 1066", "~1066".
const ABOUT: [&str; 9] = ["c.", "ca.", "circa", "about", "around", "approximately", "approx.", "~", "c"];

/// How long a day is and how it divides: every calendar has hours, minutes and
/// seconds (Joe, 2026-11-05), each of any count.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DayShape {
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
}

impl DayShape {
    pub fn seconds_in_day(self) -> i64 {
        i64::from(self.hours.max(1)) * i64::from(self.minutes.max(1)) * i64::from(self.seconds.max(1))
    }
    pub fn seconds_in_hour(self) -> i64 {
        i64::from(self.minutes.max(1)) * i64::from(self.seconds.max(1))
    }
    pub fn seconds_in_minute(self) -> i64 {
        i64::from(self.seconds.max(1))
    }
}

/// One part of the year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entry {
    Month {
        month: String,
        #[serde(default)]
        short: Option<String>,
        days: u32,
        /// Days it gains in a leap year (February's one).
        #[serde(default)]
        leap_days: u32,
    },
    /// A day of no month (Yule, Mid-year's Day); `leap` when it is there only
    /// in leap years.
    Day {
        day: String,
        #[serde(default)]
        leap: bool,
    },
}

impl Entry {
    fn days(&self, leap: bool) -> i64 {
        match self {
            Entry::Month { days, leap_days, .. } => i64::from(*days) + if leap { i64::from(*leap_days) } else { 0 },
            Entry::Day { leap: only_leap, .. } => i64::from(!*only_leap || leap),
        }
    }
}

/// Which years are leap years: one rule from a small fixed set, tested on
/// `Y + offset` (the Gregorian rule is tested on the astronomical year,
/// `Y + 1`, so 1 BC is a leap year).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Leap {
    /// Every this many years (0: never).
    #[serde(default)]
    pub every: i64,
    /// ... except every this many (0: no exception).
    #[serde(default)]
    pub except_every: i64,
    /// ... except every this many after all.
    #[serde(default)]
    pub except_except_every: i64,
    #[serde(default)]
    pub offset: i64,
}

impl Leap {
    pub fn is_leap(&self, year: i64) -> bool {
        let a = year + self.offset;
        let div = |n: i64| n > 0 && a.rem_euclid(n) == 0;
        div(self.every) && !(div(self.except_every) && !div(self.except_except_every))
    }

    /// Years after which the rule repeats.
    fn period(&self) -> i64 {
        [self.except_except_every, self.except_every, self.every].into_iter().find(|n| *n > 0).unwrap_or(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Counts {
    Up,
    Down,
}

/// How a person numbers years: an era counting up from its first year (`from`)
/// or down to its last (`to`), each an absolute year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Era {
    pub name: String,
    pub short: String,
    /// Other ways of writing it ("BCE", "B.C.").
    #[serde(default)]
    pub also: Vec<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
    pub counts: Counts,
    /// Left out when writing (AD), and the era a year typed without one is in.
    #[serde(default)]
    pub quiet: bool,
}

/// How exactly a typed moment was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    Year,
    Month,
    Day,
    Minute,
    Second,
}

/// A moment read from text: where it starts on the line, and to what
/// precision it was given ("1066" is a year, "Oct 14 1066" a day).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    pub at: i64,
    pub precision: Precision,
}

/// A day of a calendar's year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// The absolute year.
    pub year: i64,
    /// The entry of the year (a month, or a day of no month).
    pub entry: usize,
    /// The day of that entry, from 1.
    pub day: u32,
    /// Seconds into the day.
    pub second: i64,
}

impl Calendar {
    /// The Gregorian calendar, BC and AD with no year zero, proleptic before
    /// 1582: the default of every timeline.
    pub fn gregorian() -> Calendar {
        let m = |name: &str, short: &str, days: u32, leap_days: u32| Entry::Month { month: name.into(), short: Some(short.into()), days, leap_days };
        Calendar {
            name: "Gregorian".into(),
            day: DayShape { hours: 24, minutes: 60, seconds: 60 },
            week: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"].map(String::from).to_vec(),
            week_starts: 0,
            year: vec![
                m("January", "Jan", 31, 0),
                m("February", "Feb", 28, 1),
                m("March", "Mar", 31, 0),
                m("April", "Apr", 30, 0),
                m("May", "May", 31, 0),
                m("June", "Jun", 30, 0),
                m("July", "Jul", 31, 0),
                m("August", "Aug", 31, 0),
                m("September", "Sep", 30, 0),
                m("October", "Oct", 31, 0),
                m("November", "Nov", 30, 0),
                m("December", "Dec", 31, 0),
            ],
            leap: Leap { every: 4, except_every: 100, except_except_every: 400, offset: 1 },
            epoch: 0,
            eras: vec![
                Era { name: "Before Christ".into(), short: "BC".into(), also: vec!["BCE".into()], from: None, to: Some(-1), counts: Counts::Down, quiet: false },
                Era { name: "Anno Domini".into(), short: "AD".into(), also: vec!["CE".into()], from: Some(0), to: None, counts: Counts::Up, quiet: true },
            ],
            seasons: vec![
                Season { name: "Spring".into(), also: Vec::new(), from: "March".into(), to: "May".into() },
                Season { name: "Summer".into(), also: Vec::new(), from: "June".into(), to: "August".into() },
                Season { name: "Autumn".into(), also: vec!["Fall".into()], from: "September".into(), to: "November".into() },
                Season { name: "Winter".into(), also: Vec::new(), from: "December".into(), to: "February".into() },
            ],
            writes: Writes::default(),
        }
    }

    pub fn seconds_in_day(&self) -> i64 {
        self.day.seconds_in_day()
    }

    pub fn year_days(&self, year: i64) -> i64 {
        let leap = self.leap.is_leap(year);
        self.year.iter().map(|e| e.days(leap)).sum::<i64>().max(1)
    }

    fn cycle(&self) -> (i64, i64) {
        let period = self.leap.period();
        (period, (0..period).map(|y| self.year_days(y)).sum())
    }

    /// Days from the start of year 0 to the start of `year`.
    pub fn days_before(&self, year: i64) -> i64 {
        let (period, cycle_days) = self.cycle();
        let (q, r) = (year.div_euclid(period), year.rem_euclid(period));
        let base = q * period;
        q * cycle_days + (0..r).map(|k| self.year_days(base + k)).sum::<i64>()
    }

    /// The year holding day `day` (from the start of year 0), and the day
    /// within it, from 0.
    pub fn year_of_day(&self, day: i64) -> (i64, i64) {
        let (period, cycle_days) = self.cycle();
        let q = day.div_euclid(cycle_days);
        let mut rem = day.rem_euclid(cycle_days);
        let mut year = q * period;
        loop {
            let len = self.year_days(year);
            if rem < len {
                return (year, rem);
            }
            rem -= len;
            year += 1;
        }
    }

    /// Where `year` begins on the line.
    pub fn year_start(&self, year: i64) -> i64 {
        self.epoch.saturating_add(self.days_before(year).saturating_mul(self.seconds_in_day()))
    }

    /// The day `at` falls on.
    pub fn date_of(&self, at: i64) -> Date {
        let sid = self.seconds_in_day();
        let since = at - self.epoch;
        let (day, second) = (since.div_euclid(sid), since.rem_euclid(sid));
        let (year, mut in_year) = self.year_of_day(day);
        let leap = self.leap.is_leap(year);
        for (entry, e) in self.year.iter().enumerate() {
            let len = e.days(leap);
            if in_year < len {
                return Date { year, entry, day: in_year as u32 + 1, second };
            }
            in_year -= len;
        }
        Date { year, entry: self.year.len().saturating_sub(1), day: 1, second }
    }

    /// Where a day of the calendar begins on the line, plus `second`.
    pub fn moment_of(&self, date: Date) -> i64 {
        let leap = self.leap.is_leap(date.year);
        let before: i64 = self.year.iter().take(date.entry).map(|e| e.days(leap)).sum();
        let day = self.days_before(date.year) + before + i64::from(date.day.max(1)) - 1;
        self.epoch.saturating_add(day.saturating_mul(self.seconds_in_day())).saturating_add(date.second)
    }

    /// How a person numbers absolute year `year`: the era it is in, and its
    /// number there. A year no era covers is numbered from the nearest.
    pub fn era_year(&self, year: i64) -> (Option<&Era>, i64) {
        let covers = |e: &Era| e.from.map_or(true, |f| year >= f) && e.to.map_or(true, |t| year <= t);
        let era = self.eras.iter().find(|e| covers(e)).or_else(|| self.eras.first());
        match era {
            Some(e) => (Some(e), Self::number_in(e, year)),
            None => (None, year),
        }
    }

    fn number_in(era: &Era, year: i64) -> i64 {
        match era.counts {
            Counts::Up => year - era.from.unwrap_or(0) + 1,
            Counts::Down => era.to.unwrap_or(0) - year + 1,
        }
    }

    fn absolute_in(era: &Era, n: i64) -> i64 {
        match era.counts {
            Counts::Up => era.from.unwrap_or(0) + n - 1,
            Counts::Down => era.to.unwrap_or(0) - n + 1,
        }
    }

    /// A year as written: "1066", "44 BC".
    pub fn year_label(&self, year: i64) -> String {
        match self.era_year(year) {
            (Some(e), n) if !e.quiet => format!("{n} {}", e.short),
            (_, n) => n.to_string(),
        }
    }

    /// An entry's name as written: a month's short name, or a day's name.
    pub fn entry_label(&self, entry: usize) -> String {
        match self.year.get(entry) {
            Some(Entry::Month { month, short, .. }) => short.clone().unwrap_or_else(|| month.clone()),
            Some(Entry::Day { day, .. }) => day.clone(),
            None => String::new(),
        }
    }

    pub fn is_month(&self, entry: usize) -> bool {
        matches!(self.year.get(entry), Some(Entry::Month { .. }))
    }

    /// An entry's name as a date is written: a month's short name, or its
    /// whole name when the calendar writes them whole.
    fn entry_word(&self, entry: usize) -> String {
        match self.year.get(entry) {
            Some(Entry::Month { month, .. }) if self.writes.full_months => month.clone(),
            _ => self.entry_label(entry),
        }
    }

    /// The day of the week `at` falls on, when the calendar has weeks.
    pub fn weekday(&self, at: i64) -> Option<&str> {
        let n = self.week.len() as i64;
        if n == 0 {
            return None;
        }
        let day = (at - self.epoch).div_euclid(self.seconds_in_day());
        self.week.get((day + self.week_starts as i64).rem_euclid(n) as usize).map(String::as_str)
    }

    /// A day as written: "Oct 14, 1066", "Mid-year's Day, 1420", or as the
    /// calendar's `writes` says ("Saturday, 14 October 1066").
    pub fn day_label(&self, date: Date) -> String {
        let w = self.writes;
        let (entry, year) = (self.entry_word(date.entry), self.year_label(date.year));
        let day = match (self.is_month(date.entry), w.day_first) {
            (true, false) => format!("{entry} {}, {year}", date.day),
            (true, true) => format!("{} {entry} {year}", date.day),
            (false, false) => format!("{entry}, {year}"),
            (false, true) => format!("{entry} {year}"),
        };
        match self.weekday(self.moment_of(Date { second: 0, ..date })) {
            Some(name) if w.weekday => format!("{name}, {day}"),
            _ => day,
        }
    }

    /// A time of day as written: "14:05", or "14:05:09" when it has seconds.
    pub fn time_label(&self, second: i64, with_seconds: bool) -> String {
        let (h, rest) = (second / self.day.seconds_in_hour(), second % self.day.seconds_in_hour());
        let (m, s) = (rest / self.day.seconds_in_minute(), rest % self.day.seconds_in_minute());
        if with_seconds {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{h:02}:{m:02}")
        }
    }

    /// A moment written to its precision.
    pub fn write(&self, m: Moment) -> String {
        let date = self.date_of(m.at);
        match m.precision {
            Precision::Year => self.year_label(date.year),
            Precision::Month => format!("{} {}", self.entry_word(date.entry), self.year_label(date.year)),
            Precision::Day => self.day_label(date),
            Precision::Minute => format!("{} {}", self.day_label(date), self.time_label(date.second, false)),
            Precision::Second => format!("{} {}", self.day_label(date), self.time_label(date.second, true)),
        }
    }

    /// Where the span a moment names ends: the start of the next year, month,
    /// day, minute or second.
    pub fn end_of(&self, m: Moment) -> i64 {
        let date = self.date_of(m.at);
        match m.precision {
            Precision::Year => self.year_start(date.year + 1),
            Precision::Month => {
                let leap = self.leap.is_leap(date.year);
                let len = self.year.get(date.entry).map_or(1, |e| e.days(leap));
                self.moment_of(Date { day: 1, second: 0, ..date }) + len * self.seconds_in_day()
            }
            Precision::Day => self.moment_of(Date { second: 0, ..date }) + self.seconds_in_day(),
            Precision::Minute => m.at + self.day.seconds_in_minute(),
            Precision::Second => m.at + 1,
        }
    }

    /// Read a moment from text as a person types it in this calendar: "1066",
    /// "44 BC", "Oct 1066", "Oct 14, 1066", "14 October 1066", "1066-10-14",
    /// "10/14/1066", a time after the date ("14:30", "2:30 pm", "14:30:05"),
    /// a day of no month by its name ("Mid-year's Day 1420"). A year is
    /// needed: nothing is guessed. `None` for text that is not a moment here.
    pub fn read(&self, text: &str) -> Option<Moment> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let lower = text.to_lowercase();
        // A day of no month, by its whole name, first.
        let mut rest = lower.clone();
        let mut entry: Option<usize> = None;
        for (i, e) in self.year.iter().enumerate() {
            if let Entry::Day { day, .. } = e {
                let name = day.to_lowercase();
                if let Some(at) = rest.find(&name) {
                    rest = format!("{} {}", &rest[..at], &rest[at + name.len()..]);
                    entry = Some(i);
                    break;
                }
            }
        }
        // ISO: 1066-10-14, with a time after a T or a space.
        let mut tokens: Vec<String> = Vec::new();
        for raw in rest.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
            let (date, time) = match raw.split_once('t') {
                Some((d, t)) if d.contains('-') && t.contains(':') => (d, Some(t)),
                _ => (raw, None),
            };
            tokens.push(date.to_string());
            if let Some(t) = time {
                tokens.push(t.trim_end_matches('z').to_string());
            }
        }
        let mut era: Option<&Era> = None;
        let mut month: Option<usize> = entry;
        let mut numbers: Vec<i64> = Vec::new();
        let mut time: Option<i64> = None;
        let mut time_has_seconds = false;
        let mut pm: Option<bool> = None;
        let mut iso_date: Option<(i64, usize, u32)> = None;
        let mut i = 0;
        while i < tokens.len() {
            let t = tokens[i].clone();
            let bare: String = t.chars().filter(|c| *c != '.').collect();
            i += 1;
            if bare == "am" || bare == "pm" {
                pm = Some(bare == "pm");
                continue;
            }
            if let Some(e) = self.eras.iter().find(|e| {
                let names = std::iter::once(e.short.as_str()).chain(std::iter::once(e.name.as_str())).chain(e.also.iter().map(String::as_str));
                names.map(|n| n.to_lowercase().replace('.', "")).any(|n| n == bare)
            }) {
                era = Some(e);
                continue;
            }
            if t.contains(':') {
                let parts: Vec<i64> = t.split(':').map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?;
                let (h, m, s) = match parts[..] {
                    [h, m] => (h, m, 0),
                    [h, m, s] => {
                        time_has_seconds = true;
                        (h, m, s)
                    }
                    _ => return None,
                };
                time = Some(h * self.day.seconds_in_hour() + m * self.day.seconds_in_minute() + s);
                continue;
            }
            // 1066-10-14 (a leading minus is an astronomical year: -43 is 44 BC).
            let (negative, body) = match t.strip_prefix('-') {
                Some(b) => (true, b),
                None => (false, t.as_str()),
            };
            if body.contains('-') {
                let parts: Vec<i64> = body.split('-').map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?;
                let [y, m, d] = parts[..] else { return None };
                let year = if negative { -y } else { y };
                iso_date = Some((year, self.month_index(m as usize)?, d as u32));
                continue;
            }
            if t.contains('/') {
                let parts: Vec<i64> = t.split('/').map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?;
                let [m, d, y] = parts[..] else { return None };
                month = Some(self.month_index(m as usize)?);
                numbers.push(d);
                numbers.push(y);
                continue;
            }
            if let Ok(n) = t.parse::<i64>() {
                numbers.push(n);
                continue;
            }
            if let Some(m) = self.month_named(&bare) {
                month = Some(m);
                continue;
            }
            // A day of the week before the date says nothing more.
            if self.week.iter().any(|w| {
                let w = w.to_lowercase();
                bare == w || (bare.len() >= 3 && w.starts_with(&bare))
            }) {
                continue;
            }
            return None;
        }
        if let Some(pm) = pm {
            let half = self.day.seconds_in_day() / 2;
            let t = time.get_or_insert(0);
            if *t >= half + self.day.seconds_in_hour() {
                return None;
            }
            let hour12 = *t / self.day.seconds_in_hour();
            if hour12 == self.day.hours as i64 / 2 {
                *t -= half;
            }
            if pm {
                *t += half;
            }
        }
        // The year as absolute: ISO years are the quiet era's (AD) numbering
        // directly; a year with an era is numbered in it; one without, in the
        // quiet era (or the last).
        let to_absolute = |n: i64| -> i64 {
            let e = era.or_else(|| self.eras.iter().find(|e| e.quiet)).or_else(|| self.eras.last());
            e.map_or(n, |e| Self::absolute_in(e, n))
        };
        let (year, entry, day, precision) = if let Some((y, m, d)) = iso_date {
            let absolute = match era {
                Some(e) => Self::absolute_in(e, y),
                // Astronomical numbering: 0 is 1 BC, which is absolute -1 in a
                // calendar whose quiet era starts at 0.
                None if y <= 0 => self.eras.iter().find(|e| e.quiet).map_or(y, |e| e.from.unwrap_or(0) + y - 1),
                None => to_absolute(y),
            };
            (absolute, m, d, Precision::Day)
        } else {
            match (month, &numbers[..]) {
                (None, [y]) => (to_absolute(*y), 0, 1, Precision::Year),
                (Some(m), [y]) if !self.is_month(m) => (to_absolute(*y), m, 1, Precision::Day),
                // "Oct 14" is a day with no year more often than October of
                // AD 14: a small number beside a month needs its era.
                (Some(_), [n]) if era.is_none() && (1..=31).contains(n) => return None,
                (Some(m), [y]) => (to_absolute(*y), m, 1, Precision::Month),
                (Some(m), [a, b]) => {
                    // A day is the small number written next to the month.
                    let (d, y) = if (1..=31).contains(a) && !(1..=31).contains(b) { (*a, *b) } else if (1..=31).contains(b) && !(1..=31).contains(a) { (*b, *a) } else { (*a, *b) };
                    (to_absolute(y), m, d as u32, Precision::Day)
                }
                _ => return None,
            }
        };
        let leap = self.leap.is_leap(year);
        let len = self.year.get(entry)?.days(leap);
        if day == 0 || i64::from(day) > len {
            return None;
        }
        let precision = match (time, precision) {
            (Some(_), Precision::Day) if time_has_seconds => Precision::Second,
            (Some(_), Precision::Day) => Precision::Minute,
            (Some(_), _) => return None,
            (None, p) => p,
        };
        let second = time.unwrap_or(0);
        if second < 0 || second >= self.seconds_in_day() {
            return None;
        }
        Some(Moment { at: self.moment_of(Date { year, entry, day, second }), precision })
    }

    /// The era called `word` (its short name, name or another spelling,
    /// dots ignored).
    fn era_named(&self, word: &str) -> Option<&Era> {
        let bare: String = word.chars().filter(|c| *c != '.').collect::<String>().to_lowercase();
        self.eras.iter().find(|e| {
            let names = std::iter::once(e.short.as_str()).chain(std::iter::once(e.name.as_str())).chain(e.also.iter().map(String::as_str));
            names.map(|n| n.to_lowercase().replace('.', "")).any(|n| n == bare)
        })
    }

    /// The era a year written with `era_words` (perhaps none) is in.
    fn era_of_words(&self, era_words: &str) -> Option<&Era> {
        let w = era_words.trim();
        if w.is_empty() {
            self.eras.iter().find(|e| e.quiet).or_else(|| self.eras.last())
        } else {
            self.era_named(w)
        }
    }

    /// What a field says (docs/TIMELINE_CONTRACT.md "Fuzzy dates"): a moment
    /// as [`Calendar::read`] reads one; "c. 1066" (about); "412-415 AF",
    /// "1939 to 1945" (two moments in one field); "early 1400s", "the
    /// 1960s", "the 15th century BC", "spring of 412", "late Oct 1066"
    /// (rough). `None` for text that says none of these here.
    pub fn read_said(&self, text: &str) -> Option<Said> {
        let lower = text.trim().to_lowercase();
        let (about, rest) = strip_about(&lower);
        if let Some(said) = self.read_rough(rest, about) {
            return Some(said);
        }
        if let Some((from, to)) = self.read_between(rest) {
            return Some(Said::Between { from, to, about });
        }
        self.read(rest).map(|moment| Said::At { moment, about })
    }

    /// Two moments in one field: "1939 to 1945", "Oct 1 to Oct 14, 1066",
    /// "412-415 AF", "44 BC – AD 14". An era or a year written only after the
    /// second is the first's too.
    fn read_between(&self, text: &str) -> Option<(Moment, Moment)> {
        let text = text.trim().trim_start_matches("between ").trim_start_matches("from ");
        let parts = [" to ", " until ", " and ", " \u{2013} ", " \u{2014} ", "\u{2013}", "\u{2014}", " - "].iter().find_map(|sep| text.split_once(sep));
        let (left, right) = match parts {
            Some((l, r)) => (l.trim().to_string(), r.trim().to_string()),
            None => {
                // "412-415 AF": a token of two whole numbers, the rest shared.
                let mut words = text.split_whitespace();
                let first = words.next()?;
                let rest: Vec<&str> = words.collect();
                let (a, b) = first.split_once('-')?;
                if a.is_empty() || !a.chars().all(|c| c.is_ascii_digit()) || !b.chars().all(|c| c.is_ascii_digit()) || b.is_empty() {
                    return None;
                }
                let (na, nb): (i64, i64) = (a.parse().ok()?, b.parse().ok()?);
                if nb < na {
                    return None;
                }
                let tail = rest.join(" ");
                (format!("{a} {tail}"), format!("{b} {tail}"))
            }
        };
        let to = self.read(&right)?;
        let tail = right.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).last().unwrap_or("");
        let left_has_era = left.split(|c: char| c.is_whitespace() || c == ',').any(|w| self.era_named(w).is_some());
        let borrowed = format!("{left} {tail}");
        let from = if self.era_named(tail).is_some() && !left_has_era {
            self.read(&borrowed)
        } else {
            self.read(&left).or_else(|| self.read(&borrowed))
        }?;
        Some((from, to))
    }

    /// A rough part of time: early, mid or late anything that reads, a
    /// decade or century ("1960s", "1400s", "the 15th century"), a season of
    /// a year.
    fn read_rough(&self, text: &str, about: bool) -> Option<Said> {
        let text = text.trim().trim_start_matches("the ").trim();
        let parts = [("early ", 0u8), ("mid-", 1), ("mid ", 1), ("middle of ", 1), ("late ", 2)];
        let (part, text) = match parts.iter().find_map(|(w, p)| text.strip_prefix(w).map(|r| (Some(*p), r))) {
            Some((p, r)) => (p, r.trim().trim_start_matches("the ").trim_start_matches("of ").trim()),
            None => (None, text),
        };
        let period = self.read_period(text).or_else(|| {
            // Early, mid or late anything else that reads.
            let _ = part?;
            match self.read_between(text) {
                Some((a, b)) => Some((a.at, self.end_of(b))),
                None => self.read(text).map(|m| (m.at, self.end_of(m))),
            }
        })?;
        let (from, to) = match part {
            None => period,
            Some(p) => {
                let third = (period.1 - period.0) / 3;
                let from = period.0 + third * i64::from(p);
                (from, if p == 2 { period.1 } else { from + third })
            }
        };
        Some(Said::Rough { from, to, soft: about || part.is_some() })
    }

    /// A decade, century or season: "1960s", "1400s BC", "15th century",
    /// "spring of 412 AF". `None` for anything else.
    fn read_period(&self, text: &str) -> Option<(i64, i64)> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let first = *words.first()?;
        let rest = words[1..].join(" ");
        // "1960s", "1400s", "1960's".
        let digits = first.strip_suffix("'s").or_else(|| first.strip_suffix('s'));
        if let Some(n) = digits.filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())).and_then(|d| d.parse::<i64>().ok()) {
            let size = if n % 100 == 0 { 100 } else if n % 10 == 0 { 10 } else { return None };
            return self.years_span(n, n + size - 1, &rest);
        }
        // "15th century BC".
        let ordinal = ["st", "nd", "rd", "th"].iter().find_map(|s| first.strip_suffix(s)).filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
        if let (Some(n), Some(after)) = (ordinal.and_then(|d| d.parse::<i64>().ok()), rest.strip_prefix("century")) {
            if n < 1 {
                return None;
            }
            return self.years_span((n - 1) * 100 + 1, n * 100, after);
        }
        // "spring of 412", "spring 412 AF".
        let season = self.seasons.iter().position(|s| {
            std::iter::once(&s.name).chain(s.also.iter()).any(|n| n.to_lowercase() == first)
        })?;
        let year_text = rest.trim_start_matches("of ").trim();
        let m = self.read(year_text)?;
        if m.precision != Precision::Year {
            return None;
        }
        self.season_span(season, self.date_of(m.at).year)
    }

    /// Years `a` to `b` as numbered in the era `era_words` names (none: the
    /// quiet one), from the start of the earlier to the end of the later.
    fn years_span(&self, a: i64, b: i64, era_words: &str) -> Option<(i64, i64)> {
        let era = self.era_of_words(era_words)?;
        let (x, y) = (Self::absolute_in(era, a.max(1)), Self::absolute_in(era, b.max(1)));
        let (lo, hi) = (x.min(y), x.max(y));
        Some((self.year_start(lo), self.year_start(hi + 1)))
    }

    /// Where season `i` of absolute year `year` starts and ends: into the
    /// next year when it wraps (winter).
    fn season_span(&self, i: usize, year: i64) -> Option<(i64, i64)> {
        let s = self.seasons.get(i)?;
        let (from, to) = (self.month_named(&s.from.to_lowercase())?, self.month_named(&s.to.to_lowercase())?);
        let to_year = if to < from { year + 1 } else { year };
        let start = self.moment_of(Date { year, entry: from, day: 1, second: 0 });
        let end = self.end_of(Moment { at: self.moment_of(Date { year: to_year, entry: to, day: 1, second: 0 }), precision: Precision::Month });
        Some((start, end))
    }

    /// Where an "about" moment's soft span starts and ends: a unit of its
    /// own either side ("c. Oct 14, 1066" a day), and for a year more the
    /// rounder it is ("c. 1066" a year, "c. 1060" five, "c. 300 BC" fifty).
    pub fn about_span(&self, m: Moment) -> (i64, i64) {
        let k = match m.precision {
            Precision::Year => match self.era_year(self.date_of(m.at).year).1 {
                n if n % 100 == 0 => 50,
                n if n % 10 == 0 => 5,
                _ => 1,
            },
            _ => 1,
        };
        // Whole units of the calendar's own, so a leap year counts as one.
        let (mut from, mut to) = (m.at, self.end_of(m));
        for _ in 0..k {
            from = self.floor(from.saturating_sub(1), m.precision);
            to = self.end_of(Moment { at: to, precision: m.precision });
        }
        (from, to)
    }

    /// Two moments written in one field: "412\u{2013}415 AF" when both are
    /// years of one era, else "Oct 1, 1066 to Oct 14, 1066".
    pub fn write_between(&self, from: Moment, to: Moment) -> String {
        if from.precision == Precision::Year && to.precision == Precision::Year {
            let (a, b) = (self.era_year(self.date_of(from.at).year), self.era_year(self.date_of(to.at).year));
            if a.0.map(|e| &e.name) == b.0.map(|e| &e.name) {
                return match a.0 {
                    Some(e) if !e.quiet => format!("{}\u{2013}{} {}", a.1, b.1, e.short),
                    _ => format!("{}\u{2013}{}", a.1, b.1),
                };
            }
        }
        format!("{} to {}", self.write(from), self.write(to))
    }

    /// A moment written with "c." when it is about.
    pub fn write_about(&self, m: Moment, about: bool) -> String {
        if about {
            format!("c. {}", self.write(m))
        } else {
            self.write(m)
        }
    }

    /// The entry of the `n`th month (from 1), skipping days of no month.
    fn month_index(&self, n: usize) -> Option<usize> {
        self.year.iter().enumerate().filter(|(_, e)| matches!(e, Entry::Month { .. })).nth(n.checked_sub(1)?).map(|(i, _)| i)
    }

    /// The month called `word` (its name, short name, or three letters of it).
    fn month_named(&self, word: &str) -> Option<usize> {
        self.year.iter().position(|e| match e {
            Entry::Month { month, short, .. } => {
                let (name, short) = (month.to_lowercase(), short.as_deref().map(str::to_lowercase));
                word == name || short.as_deref() == Some(word) || (word.len() >= 3 && name.starts_with(word))
            }
            Entry::Day { .. } => false,
        })
    }

    /// A calendar to make one's own from: twelve months of thirty days, a
    /// day of 24 hours, years counted up from year 1 of one era that starts
    /// where the Gregorian AD 1 does.
    pub fn invented() -> Calendar {
        let names = ["First", "Second", "Third", "Fourth", "Fifth", "Sixth", "Seventh", "Eighth", "Ninth", "Tenth", "Eleventh", "Twelfth"];
        Calendar {
            name: "Untitled calendar".into(),
            day: DayShape { hours: 24, minutes: 60, seconds: 60 },
            week: Vec::new(),
            week_starts: 0,
            year: names
                .iter()
                .map(|n| Entry::Month { month: format!("{n} Month"), short: Some(n[..3].to_string()), days: 30, leap_days: 0 })
                .collect(),
            leap: Leap::default(),
            epoch: 0,
            eras: vec![Era { name: "Year of the Founding".into(), short: "YF".into(), also: Vec::new(), from: Some(0), to: None, counts: Counts::Up, quiet: false }],
            seasons: Vec::new(),
            writes: Writes::default(),
        }
    }

    /// The calendar as a sentence names it: "the Gregorian calendar", "the
    /// Untitled calendar", "the Shire Reckoning calendar".
    pub fn called(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            "this calendar".into()
        } else if name.to_lowercase().ends_with("calendar") {
            format!("the {name}")
        } else {
            format!("the {name} calendar")
        }
    }

    /// What makes this calendar one the line cannot use yet, as a sentence:
    /// what a calendar being made up shows until it can be read and written
    /// in. `None` when it is usable.
    pub fn problem(&self) -> Option<String> {
        let d = self.day;
        for (n, what) in [(d.hours, "hours"), (d.minutes, "minutes"), (d.seconds, "seconds")] {
            if !(1..=10_000).contains(&n) {
                return Some(format!("A day needs between 1 and 10,000 {what}."));
            }
        }
        if self.year.is_empty() {
            return Some("A year needs at least one month or day.".into());
        }
        if self.year.len() > 1000 {
            return Some("A year can have at most 1,000 months and days.".into());
        }
        for e in &self.year {
            match e {
                Entry::Month { month, days, leap_days, .. } => {
                    if month.trim().is_empty() {
                        return Some("Every month needs a name.".into());
                    }
                    if *days == 0 || *days > 100_000 || *leap_days > 100_000 {
                        return Some(format!("{month} needs between 1 and 100,000 days."));
                    }
                }
                Entry::Day { day, .. } if day.trim().is_empty() => return Some("Every day of no month needs a name.".into()),
                Entry::Day { .. } => {}
            }
        }
        if !self.year.iter().any(|e| e.days(false) > 0) {
            return Some("A year needs at least one day that is not only in leap years.".into());
        }
        let l = self.leap;
        if [l.every, l.except_every, l.except_except_every].iter().any(|n| !(0..=10_000).contains(n)) {
            return Some("Leap years repeat every 10,000 years at most.".into());
        }
        if self.eras.is_empty() {
            return Some("A calendar needs at least one era to number its years.".into());
        }
        for e in &self.eras {
            if e.name.trim().is_empty() && e.short.trim().is_empty() {
                return Some("Every era needs a name.".into());
            }
            if e.counts == Counts::Down && e.to.is_none() {
                return Some(format!("{} counts down, so it needs the year it ends.", e.name));
            }
            if let (Some(f), Some(t)) = (e.from, e.to) {
                if t < f {
                    return Some(format!("{} ends before it begins.", e.name));
                }
            }
        }
        None
    }

    /// A calendar as stored in a calendar node's `data` (its JSON), checked:
    /// `Err` says what keeps it from being used.
    pub fn from_json(value: &serde_json::Value) -> Result<Calendar, String> {
        let cal: Calendar = serde_json::from_value(value.clone()).map_err(|_| "This calendar is not finished: it is missing parts a calendar needs.".to_string())?;
        match cal.problem() {
            Some(p) => Err(p),
            None => Ok(cal),
        }
    }

    /// Where the year, month, day, minute or second holding `at` begins.
    pub fn floor(&self, at: i64, precision: Precision) -> i64 {
        let date = self.date_of(at);
        match precision {
            Precision::Year => self.year_start(date.year),
            Precision::Month => self.moment_of(Date { day: 1, second: 0, ..date }),
            Precision::Day => self.moment_of(Date { second: 0, ..date }),
            Precision::Minute => at - date.second.rem_euclid(self.day.seconds_in_minute()),
            Precision::Second => at,
        }
    }

    /// The start of the year, month, day, minute or second nearest `at`: where
    /// a moment dragged to `at` lands, kept to the precision it was given.
    pub fn round(&self, at: i64, precision: Precision) -> i64 {
        let down = self.floor(at, precision);
        let up = self.end_of(Moment { at: down, precision });
        if at - down > up - at {
            up
        } else {
            down
        }
    }

    /// A moment from a person's own clock: what a typed time of now reads as
    /// in this calendar (the line's second is a real second).
    pub fn now() -> i64 {
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        UNIX_EPOCH_ON_LINE + unix
    }
}

/// Text with a leading "about" ("c.", "circa", "~") taken off, and whether
/// it had one.
fn strip_about(text: &str) -> (bool, &str) {
    for w in ABOUT {
        if let Some(rest) = text.strip_prefix(w) {
            // A word, not the start of one ("cafe", "around" is not "a").
            let boundary = w.ends_with('.') || w == "~" || rest.starts_with(char::is_whitespace);
            if boundary && !rest.trim().is_empty() {
                return (true, rest.trim());
            }
        }
    }
    (false, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> Calendar {
        Calendar::gregorian()
    }

    #[test]
    fn a_calendar_says_what_keeps_it_from_being_used() {
        assert_eq!(g().problem(), None);
        assert_eq!(Calendar::invented().problem(), None);
        let json = serde_json::to_value(g()).unwrap();
        assert_eq!(Calendar::from_json(&json), Ok(g()), "the Gregorian calendar round-trips as JSON");
        let mut c = Calendar::invented();
        c.day.hours = 0;
        assert_eq!(c.problem().as_deref(), Some("A day needs between 1 and 10,000 hours."));
        let mut c = Calendar::invented();
        c.year.clear();
        assert_eq!(c.problem().as_deref(), Some("A year needs at least one month or day."));
        let mut c = Calendar::invented();
        c.eras[0].counts = Counts::Down;
        assert!(c.problem().unwrap().contains("counts down"));
        assert!(Calendar::from_json(&serde_json::json!({"name": "half"})).is_err());
    }

    #[test]
    fn the_line_starts_at_ad_1_and_reaches_1970_where_unix_time_does() {
        let c = g();
        assert_eq!(c.year_start(0), 0, "AD 1 is year 0");
        let d = c.date_of(UNIX_EPOCH_ON_LINE);
        assert_eq!((c.year_label(d.year), c.entry_label(d.entry), d.day, d.second), ("1970".into(), "Jan".into(), 1, 0));
        assert_eq!(c.read("1970-01-01").unwrap().at, UNIX_EPOCH_ON_LINE);
    }

    #[test]
    fn leap_years_follow_the_gregorian_rule_and_1_bc_is_one() {
        let c = g();
        let leap = |ad: i64| c.leap.is_leap(ad - 1);
        assert!(leap(2024) && leap(2000) && !leap(1900) && !leap(2026));
        assert!(c.leap.is_leap(-1), "1 BC (astronomical 0) is a leap year");
        assert_eq!(c.year_days(2023), 366);
    }

    #[test]
    fn bc_counts_down_with_no_year_zero() {
        let c = g();
        assert_eq!(c.year_label(-1), "1 BC");
        assert_eq!(c.year_label(0), "1");
        assert_eq!(c.year_label(-44), "44 BC");
        let ides = c.read("Mar 15, 44 BC").unwrap();
        assert_eq!(c.write(ides), "Mar 15, 44 BC");
        assert_eq!(c.read("15 March 44 BCE").unwrap(), ides);
        assert_eq!(c.read("-0043-03-15").unwrap(), ides, "ISO's astronomical year -43 is 44 BC");
        assert!(ides.at < 0);
        // A day before AD 1 and the day after 1 BC meet.
        assert_eq!(c.end_of(c.read("Dec 31, 1 BC").unwrap()), c.read("Jan 1, 1").unwrap().at);
    }

    #[test]
    fn moments_read_to_their_precision_and_write_back() {
        let c = g();
        let cases = [
            ("1066", Precision::Year, "1066"),
            ("Oct 1066", Precision::Month, "Oct 1066"),
            ("October 14, 1066", Precision::Day, "Oct 14, 1066"),
            ("14 Oct 1066", Precision::Day, "Oct 14, 1066"),
            ("10/14/1066", Precision::Day, "Oct 14, 1066"),
            ("1066-10-14", Precision::Day, "Oct 14, 1066"),
            ("1066-10-14T09:30", Precision::Minute, "Oct 14, 1066 09:30"),
            ("Oct 14 1066 2:30 pm", Precision::Minute, "Oct 14, 1066 14:30"),
            ("Oct 14 1066 12:05 am", Precision::Minute, "Oct 14, 1066 00:05"),
            ("Oct 14 1066 14:30:05", Precision::Second, "Oct 14, 1066 14:30:05"),
        ];
        for (typed, precision, written) in cases {
            let m = c.read(typed).unwrap_or_else(|| panic!("{typed} reads"));
            assert_eq!(m.precision, precision, "{typed}");
            assert_eq!(c.write(m), written, "{typed}");
        }
        for typed in ["", "whenever", "Oct 14", "Feb 30 2026", "2026-13-01", "25:00 Oct 1 2026"] {
            assert_eq!(c.read(typed), None, "{typed:?} is not a moment");
        }
    }

    #[test]
    fn a_span_ends_where_the_next_unit_begins() {
        let c = g();
        let m = c.read("Feb 2024").unwrap();
        assert_eq!(c.end_of(m), c.read("Mar 1 2024").unwrap().at, "February 2024 has 29 days");
        assert_eq!(c.end_of(c.read("2026").unwrap()), c.read("Jan 1 2027").unwrap().at);
        assert_eq!(c.end_of(c.read("Nov 5 2026").unwrap()) - c.read("Nov 5 2026").unwrap().at, 86_400);
    }

    #[test]
    fn an_invented_calendar_is_the_same_code() {
        // A colony calendar: 26-hour days, a 412-day year of no months but one
        // "Day" month, years After Founding, starting a million days in.
        let colony = Calendar {
            name: "Kepler".into(),
            day: DayShape { hours: 26, minutes: 60, seconds: 60 },
            week: Vec::new(),
            week_starts: 0,
            year: vec![Entry::Month { month: "Day".into(), short: None, days: 412, leap_days: 0 }, Entry::Day { day: "Founding Day".into(), leap: false }],
            leap: Leap::default(),
            epoch: 1_000_000 * 86_400,
            eras: vec![Era { name: "After Founding".into(), short: "AF".into(), also: Vec::new(), from: Some(0), to: None, counts: Counts::Up, quiet: false }],
            seasons: Vec::new(),
            writes: Writes::default(),
        };
        assert_eq!(colony.seconds_in_day(), 26 * 3600, "a real second: a longer day");
        let m = colony.read("Day 214, 3088 AF").unwrap();
        assert_eq!(colony.write(m), "Day 214, 3088 AF");
        let fest = colony.read("Founding Day 12 AF").unwrap();
        assert_eq!(colony.write(fest), "Founding Day, 12 AF");
        assert_eq!(colony.end_of(colony.read("1 AF").unwrap()) - colony.read("1 AF").unwrap().at, 413 * 26 * 3600);
        // The definition round-trips as the JSON a calendar node will hold.
        let json = serde_json::to_string(&colony).unwrap();
        assert_eq!(serde_json::from_str::<Calendar>(&json).unwrap(), colony);
    }

    #[test]
    fn a_calendar_writes_days_its_own_way_and_reads_them_back() {
        let mut c = g();
        let m = c.read("Nov 5 2026").unwrap();
        assert_eq!(c.weekday(m.at), Some("Thursday"));
        // Hastings was a Saturday in the Julian calendar of the day; the
        // proleptic Gregorian Oct 14, 1066 is six days earlier, a Sunday.
        assert_eq!(c.weekday(c.read("Oct 14 1066").unwrap().at), Some("Sunday"));
        c.writes = Writes { day_first: true, full_months: true, weekday: true };
        assert_eq!(c.write(m), "Thursday, 5 November 2026");
        assert_eq!(c.read("Thursday, 5 November 2026"), Some(m));
        assert_eq!(c.write(Moment { precision: Precision::Month, ..m }), "November 2026");
        // Which name the first day has: shifting it renames every day.
        c.week_starts = 1;
        assert_eq!(c.weekday(m.at), Some("Friday"));
    }

    #[test]
    fn rough_dates_read_as_spans_and_ranges_in_one_field_as_two_moments() {
        let c = g();
        let span = |t: &str| match c.read_said(t) {
            Some(Said::Rough { from, to, soft }) => (c.write(Moment { at: from, precision: Precision::Day }), c.write(Moment { at: to, precision: Precision::Day }), soft),
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(span("the 1960s"), ("Jan 1, 1960".into(), "Jan 1, 1970".into(), false));
        assert_eq!(span("1400s"), ("Jan 1, 1400".into(), "Jan 1, 1500".into(), false));
        assert_eq!(span("the 15th century"), ("Jan 1, 1401".into(), "Jan 1, 1501".into(), false));
        assert_eq!(span("300s BC").0, "Jan 1, 399 BC");
        assert_eq!(span("spring of 1066"), ("Mar 1, 1066".into(), "Jun 1, 1066".into(), false));
        assert_eq!(span("winter 1066"), ("Dec 1, 1066".into(), "Mar 1, 1067".into(), false), "winter runs into the next year");
        assert_eq!(span("fall of 1066").0, "Sep 1, 1066");
        let (from, to, soft) = span("early 1400s");
        assert!(soft);
        assert_eq!(from, "Jan 1, 1400");
        assert!(to.ends_with("1433"), "{to}");
        assert!(span("late Oct 1066").0.starts_with("Oct 2"), "the last third of October");
        // About: a unit either side, more for a round year.
        let Some(Said::At { moment, about: true }) = c.read_said("c. 1066") else { panic!() };
        assert_eq!(c.about_span(moment), (c.read("1065").unwrap().at, c.read("1068").unwrap().at));
        let Some(Said::At { moment, about: true }) = c.read_said("circa 300 BC") else { panic!() };
        assert_eq!(c.about_span(moment).0, c.read("350 BC").unwrap().at);
        assert!(matches!(c.read_said("~Oct 14, 1066"), Some(Said::At { about: true, .. })));
        // Two moments in one field, an era written once.
        let between = |t: &str| match c.read_said(t) {
            Some(Said::Between { from, to, .. }) => (c.write(from), c.write(to)),
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(between("1939-1945"), ("1939".into(), "1945".into()));
        assert_eq!(between("44 to 30 BC"), ("44 BC".into(), "30 BC".into()));
        assert_eq!(between("Oct 1 to Oct 14, 1066"), ("Oct 1, 1066".into(), "Oct 14, 1066".into()));
        assert_eq!(between("44 BC \u{2013} AD 14"), ("44 BC".into(), "14".into()));
        assert_eq!(c.write_between(c.read("44 BC").unwrap(), c.read("30 BC").unwrap()), "44\u{2013}30 BC");
        assert_eq!(c.write_between(c.read("1939").unwrap(), c.read("1945").unwrap()), "1939\u{2013}1945");
        assert_eq!(between("1939\u{2013}1945"), ("1939".into(), "1945".into()));
        // Still a plain date, and still nothing guessed.
        assert!(matches!(c.read_said("1066-10-14"), Some(Said::At { about: false, .. })));
        assert_eq!(c.read_said("sometime"), None);
        assert_eq!(c.read_said("c."), None);
    }
}
