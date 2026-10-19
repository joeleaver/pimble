//! Excel serial dates: whole days since 1899-12-30, so that `TODAY()-7`,
//! `EOMONTH` and comparisons are plain arithmetic.
//!
//! Excel inherited Lotus 1-2-3's belief that 1900 was a leap year: serial 60
//! is "1900-02-29" and serials 1 to 59 are one day later than the calendar
//! says. That is kept, so serials match Excel's everywhere; it only matters
//! for dates before March 1900. The calendar is proleptic Gregorian, years 0
//! to 9999, computed without the system clock (today comes from the
//! [`crate::Book`]).

/// The largest serial Excel has: 9999-12-31.
pub const MAX_SERIAL: i64 = 2_958_465;

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count from 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 1899-12-30, serial 0, as days from 1970-01-01.
const EPOCH: i64 = -25_569;

/// The serial of a date given as year, month and day, with Excel's rollover:
/// month 13 is January of the next year, day 0 the last day of the month
/// before, and so on.
pub fn serial(y: i64, m: i64, d: i64) -> i64 {
    let y = y + (m - 1).div_euclid(12);
    let m = ((m - 1).rem_euclid(12) + 1) as u32;
    let s = days_from_civil(y, m, 1) + (d - 1) - EPOCH;
    // Serials before 1900-03-01 are one more in Excel (the phantom Feb 29).
    if s < 61 {
        s - 1
    } else {
        s
    }
}

/// The year, month and day of a serial (its whole part). Serial 60 is
/// Excel's 1900-02-29 and serial 0 is shown as 1900-01-00, as Excel does.
pub fn ymd(serial: f64) -> (i64, u32, u32) {
    let s = serial.floor() as i64;
    if s == 60 {
        return (1900, 2, 29);
    }
    if s == 0 {
        return (1900, 1, 0);
    }
    let s = if s < 60 { s + 1 } else { s };
    civil_from_days(s + EPOCH)
}

/// Sunday is 0.
pub fn weekday(serial: f64) -> u32 {
    // Serial 1 (1900-01-01) was a Sunday by Excel's count.
    (serial.floor() as i64 - 1).rem_euclid(7) as u32
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

/// A strict ISO date, `YYYY-MM-DD`, as a serial.
pub fn parse_iso(s: &str) -> Option<f64> {
    let b = s.trim().as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = &b[r];
        if t.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(t).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m as u32) as i64 {
        return None;
    }
    Some(serial(y, m, d) as f64)
}

/// A serial as an ISO date.
pub fn iso(serial: f64) -> String {
    let (y, m, d) = ymd(serial);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Whether a serial is a date Excel can show.
pub fn in_range(serial: f64) -> bool {
    serial.is_finite() && serial >= 0.0 && serial < (MAX_SERIAL + 1) as f64
}

pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_serials() {
        assert_eq!(serial(1900, 1, 1), 1);
        assert_eq!(serial(1900, 2, 28), 59);
        assert_eq!(serial(1900, 3, 1), 61);
        assert_eq!(serial(1970, 1, 1), 25_569);
        assert_eq!(serial(2025, 1, 1), 45_658);
        assert_eq!(serial(2026, 10, 19), 46_314);
        assert_eq!(serial(9999, 12, 31), MAX_SERIAL);
        assert_eq!(serial(2026, 13, 1), serial(2027, 1, 1));
        assert_eq!(serial(2026, 3, 0), serial(2026, 2, 28));
    }

    #[test]
    fn round_trip() {
        for s in [1, 59, 61, 365, 25_569, 45_658, 46_314, MAX_SERIAL] {
            let (y, m, d) = ymd(s as f64);
            assert_eq!(serial(y, m as i64, d as i64), s, "{s}");
        }
        assert_eq!(ymd(60.0), (1900, 2, 29));
        assert_eq!(iso(46_314.0), "2026-10-19");
        assert_eq!(parse_iso("2026-10-19"), Some(46_314.0));
        assert_eq!(parse_iso("2026-02-30"), None);
        assert_eq!(weekday(46_314.0), 1); // a Monday
    }
}
