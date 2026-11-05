//! An item's `due` field read as a date (docs/TODO_CONTRACT.md wave 2): the way
//! a sheet's date column reads what is typed into it (`pimble_sheet::input`), so
//! "Nov 12", "11/12", "2026-11-12" and "tomorrow" mean what they mean in a
//! sheet. Text that is not a date is shown as typed, never guessed at.

use chrono::{Datelike, Duration, NaiveDate};
use pimble_sheet::input::parse_input;
use pimble_sheet::model::{ColumnType, Stored};

/// How an item's due date is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    /// "Today", "Tomorrow", "Nov 12", "Nov 12, 2027", or the text as typed.
    pub text: String,
    /// Past, and the item not done.
    pub overdue: bool,
    /// What a hover says: the date in full, or that it is not one.
    pub title: String,
}

/// The date `typed` names, read as a sheet's date column reads it.
pub fn date_of(typed: &str, today: NaiveDate) -> Option<NaiveDate> {
    match parse_input(&ColumnType::Date, typed, today, &|_| None)? {
        Stored::Date(iso) => NaiveDate::parse_from_str(&iso, "%Y-%m-%d").ok(),
        _ => None,
    }
}

/// How `typed` (an item's `due`) is shown on `today`, for an item `done` or not.
pub fn due_of(typed: &str, today: NaiveDate, done: bool) -> Due {
    let Some(date) = date_of(typed, today) else {
        return Due { text: typed.trim().to_string(), overdue: false, title: "Due (not a date this reads)".to_string() };
    };
    let text = if date == today {
        "Today".to_string()
    } else if Some(date) == today.checked_add_signed(Duration::days(1)) {
        "Tomorrow".to_string()
    } else if date.year() == today.year() {
        date.format("%b %-d").to_string()
    } else {
        date.format("%b %-d, %Y").to_string()
    };
    Due { text, overdue: !done && date < today, title: format!("Due {}", date.format("%A, %B %-d, %Y")) }
}

/// Today, on this device's clock.
pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_due_date_reads_as_a_sheet_reads_it_and_says_when_it_is_past() {
        let today = day(2026, 11, 5);
        assert_eq!(due_of("2026-11-05", today, false).text, "Today");
        assert_eq!(due_of("tomorrow", today, false).text, "Tomorrow");
        assert_eq!(due_of("Nov 12", today, false).text, "Nov 12");
        assert_eq!(due_of("1/3/2027", today, false).text, "Jan 3, 2027");
        let past = due_of("11/1", today, false);
        assert!(past.overdue && past.text == "Nov 1", "{past:?}");
        assert!(!due_of("11/1", today, true).overdue, "a done item is never overdue");
        let odd = due_of("when the moon is full", today, false);
        assert_eq!(odd.text, "when the moon is full");
        assert!(!odd.overdue);
    }
}
