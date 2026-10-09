//! Fractional indexing for `order` (docs/IMAGE_MARKUP_CONTRACT.md section 3):
//! a layer's place among layers and an element's z-order within its layer are
//! strings that sort, and a new place between two others is a new string
//! between them, so a reorder writes one key of one map and two concurrent
//! reorders of the same thing are one last-writer-wins value, never a
//! duplicate. Two places minted concurrently between the same neighbours are
//! the same string; readers break that tie by id.
//!
//! Keys are David Greenspan's "fractional-indexing" keys over base-62 digits
//! (`0-9A-Za-z`, in ASCII order, compared byte-wise): a variable-length
//! integer part whose first character says its length (`a0`..`az`,
//! `b00`..`bzz`, ...; `Z`.., `Y`.. below zero) and a fraction that never
//! ends in the zero digit. Appending or prepending steps the integer part,
//! so ten thousand elements each added on top keep keys of four characters;
//! inserting between two neighbours grows the fraction.

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: usize = 62;
/// The integer part every first key starts from.
const ZERO: &str = "a0";
/// The smallest integer part (`A` followed by 26 zero digits).
const SMALLEST: &str = "A00000000000000000000000000";

fn digit_value(c: u8) -> usize {
    DIGITS.iter().position(|d| *d == c).unwrap_or(0)
}

fn integer_length(head: u8) -> Option<usize> {
    match head {
        b'a'..=b'z' => Some((head - b'a') as usize + 2),
        b'A'..=b'Z' => Some((b'Z' - head) as usize + 2),
        _ => None,
    }
}

fn integer_part(key: &str) -> Option<&str> {
    let len = integer_length(*key.as_bytes().first()?)?;
    key.get(..len)
}

/// Whether `key` is a key this module could have made. A stored order that
/// is not is read as it sorts, and new keys are never placed against it.
pub fn is_valid(key: &str) -> bool {
    if key == SMALLEST || !key.bytes().all(|c| DIGITS.contains(&c)) {
        return false;
    }
    match integer_part(key) {
        Some(int) => !key[int.len()..].ends_with('0'),
        None => false,
    }
}

/// A key strictly between `a` and `b` (`None` = the open end). When `a` is
/// not below `b` (a tie between concurrent writers, or keys out of order),
/// the answer is a key just above `a`.
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.filter(|k| is_valid(k));
    let b = b.filter(|k| is_valid(k));
    let b = match (a, b) {
        (Some(a), Some(b)) if a >= b => None,
        (_, b) => b,
    };
    key_between(a, b)
}

/// A key after `a` (`None`: the first key).
pub fn after(a: Option<&str>) -> String {
    between(a, None)
}

/// A key before `b`.
pub fn before(b: &str) -> String {
    between(None, Some(b))
}

/// `n` keys in increasing order between `a` and `b`, evenly spread.
pub fn n_between(a: Option<&str>, b: Option<&str>, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![between(a, b)];
    }
    if b.is_none() {
        // Appending a run: step the integer part, as one append at a time would.
        let mut out = Vec::with_capacity(n);
        let mut last = a.map(str::to_string);
        for _ in 0..n {
            let k = between(last.as_deref(), None);
            out.push(k.clone());
            last = Some(k);
        }
        return out;
    }
    let mid = between(a, b);
    let mut left = n_between(a, Some(&mid), n / 2);
    let right = n_between(Some(&mid), b, n - n / 2 - 1);
    left.push(mid);
    left.extend(right);
    left
}

/// Both valid, `a < b` when both are given.
fn key_between(a: Option<&str>, b: Option<&str>) -> String {
    match (a, b) {
        (None, None) => ZERO.to_string(),
        (None, Some(b)) => {
            let ib = integer_part(b).unwrap_or(ZERO);
            let fb = &b[ib.len()..];
            if ib == SMALLEST {
                return format!("{ib}{}", midpoint(b"", Some(fb.as_bytes())));
            }
            if ib < b {
                return ib.to_string();
            }
            decrement_integer(ib).unwrap_or_else(|| format!("{ib}{}", midpoint(b"", Some(fb.as_bytes()))))
        }
        (Some(a), None) => {
            let ia = integer_part(a).unwrap_or(ZERO);
            let fa = &a[ia.len()..];
            increment_integer(ia).unwrap_or_else(|| format!("{ia}{}", midpoint(fa.as_bytes(), None)))
        }
        (Some(a), Some(b)) => {
            let ia = integer_part(a).unwrap_or(ZERO);
            let fa = &a[ia.len()..];
            let ib = integer_part(b).unwrap_or(ZERO);
            let fb = &b[ib.len()..];
            if ia == ib {
                return format!("{ia}{}", midpoint(fa.as_bytes(), Some(fb.as_bytes())));
            }
            match increment_integer(ia) {
                Some(i) if i.as_str() < b => i,
                _ => format!("{ia}{}", midpoint(fa.as_bytes(), None)),
            }
        }
    }
}

fn increment_integer(x: &str) -> Option<String> {
    let bytes = x.as_bytes();
    let head = bytes[0];
    let mut digs: Vec<u8> = bytes[1..].to_vec();
    let mut carry = true;
    for d in digs.iter_mut().rev() {
        if !carry {
            break;
        }
        let v = digit_value(*d) + 1;
        if v == BASE {
            *d = DIGITS[0];
        } else {
            *d = DIGITS[v];
            carry = false;
        }
    }
    if carry {
        if head == b'Z' {
            return Some(ZERO.to_string());
        }
        if head == b'z' {
            return None;
        }
        let h = head + 1;
        if h > b'a' {
            digs.push(DIGITS[0]);
        } else {
            digs.pop();
        }
        let mut out = vec![h];
        out.extend(digs);
        return String::from_utf8(out).ok();
    }
    let mut out = vec![head];
    out.extend(digs);
    String::from_utf8(out).ok()
}

fn decrement_integer(x: &str) -> Option<String> {
    let bytes = x.as_bytes();
    let head = bytes[0];
    let mut digs: Vec<u8> = bytes[1..].to_vec();
    let mut borrow = true;
    for d in digs.iter_mut().rev() {
        if !borrow {
            break;
        }
        let v = digit_value(*d);
        if v == 0 {
            *d = DIGITS[BASE - 1];
        } else {
            *d = DIGITS[v - 1];
            borrow = false;
        }
    }
    if borrow {
        if head == b'a' {
            return Some(format!("Z{}", DIGITS[BASE - 1] as char));
        }
        if head == b'A' {
            return None;
        }
        let h = head - 1;
        if h < b'Z' {
            digs.push(DIGITS[BASE - 1]);
        } else {
            digs.pop();
        }
        let mut out = vec![h];
        out.extend(digs);
        return String::from_utf8(out).ok();
    }
    let mut out = vec![head];
    out.extend(digs);
    String::from_utf8(out).ok()
}

/// A fraction between `a` (possibly empty) and `b`, with `a < b`.
fn midpoint(a: &[u8], b: Option<&[u8]>) -> String {
    if let Some(b) = b {
        // The common prefix, reading `a` padded with zero digits.
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(DIGITS[0]) == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = String::from_utf8(b[..n].to_vec()).unwrap();
            let rest_a = if n < a.len() { &a[n..] } else { &[][..] };
            out.push_str(&midpoint(rest_a, Some(&b[n..])));
            return out;
        }
    }
    let digit_a = a.first().map(|c| digit_value(*c)).unwrap_or(0);
    let digit_b = b.map(|b| digit_value(b[0])).unwrap_or(BASE);
    if digit_b - digit_a > 1 {
        let mid = (digit_a + digit_b).div_ceil(2);
        return (DIGITS[mid] as char).to_string();
    }
    // The first digits are adjacent.
    if let Some(b) = b {
        if b.len() > 1 {
            return (b[0] as char).to_string();
        }
    }
    let mut out = (DIGITS[digit_a] as char).to_string();
    let rest = if a.len() > 1 { &a[1..] } else { &[][..] };
    out.push_str(&midpoint(rest, None));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_between_their_neighbours() {
        let first = after(None);
        assert!(is_valid(&first));
        let mut keys = vec![first];
        // Appending, prepending and inserting in the middle, many times.
        for i in 0..500 {
            let last = keys.last().unwrap().clone();
            keys.push(after(Some(&last)));
            let head = keys[0].clone();
            keys.insert(0, before(&head));
            let mid = keys.len() / 2;
            let k = between(Some(&keys[mid - 1]), Some(&keys[mid]));
            keys.insert(mid, k);
            if i % 50 == 0 {
                for w in keys.windows(2) {
                    assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
                }
            }
        }
        for w in keys.windows(2) {
            assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
        }
        assert!(keys.iter().all(|k| is_valid(k)));
    }

    #[test]
    fn appending_grows_keys_slowly() {
        let mut last = after(None);
        for _ in 0..2000 {
            last = after(Some(&last));
        }
        assert!(last.len() <= 3, "{last}");
        let mut first = after(None);
        for _ in 0..10_000 {
            first = before(&first);
        }
        assert!(first.len() <= 4, "{first}");
    }

    #[test]
    fn repeated_insertion_at_one_point_stays_ordered() {
        let (a, mut b) = (after(None), after(Some(&after(None))));
        for _ in 0..200 {
            let k = between(Some(&a), Some(&b));
            assert!(a < k && k < b, "{a} < {k} < {b}");
            b = k;
        }
    }

    #[test]
    fn a_tie_or_reversed_pair_answers_above_the_lower() {
        let a = after(None);
        let k = between(Some(&a), Some(&a));
        assert!(k > a);
        let b = after(Some(&a));
        let k = between(Some(&b), Some(&a));
        assert!(k > b);
        // Garbage reads as an open end.
        let k = between(Some("not a key!"), None);
        assert!(is_valid(&k));
    }

    #[test]
    fn n_between_spreads_in_order() {
        let a = after(None);
        let b = after(Some(&a));
        let keys = n_between(Some(&a), Some(&b), 37);
        assert_eq!(keys.len(), 37);
        assert!(a < keys[0] && keys[36] < b);
        for w in keys.windows(2) {
            assert!(w[0] < w[1]);
        }
    }
}
