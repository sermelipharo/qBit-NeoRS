//! Natural ("human") string ordering, like qBittorrent's
//! `Utils::Compare::NaturalLessThan` (QCollator with numeric mode):
//! digit runs compare by value, everything else case-insensitively.

use std::cmp::Ordering;

/// Case-insensitive natural comparison: `"file2" < "File10"`.
/// Strings that compare equal ignoring case fall back to a byte comparison
/// so the order is total and deterministic.
pub fn natural_cmp_ci(a: &str, b: &str) -> Ordering {
    natural_cmp_inner(a, b).then_with(|| a.cmp(b))
}

fn natural_cmp_inner(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let da = take_digits(&mut ai);
                let db = take_digits(&mut bi);
                let ta = da.trim_start_matches('0');
                let tb = db.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
                // Same value: fewer leading zeros first ("1" < "01").
                let ord = da.len().cmp(&db.len());
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                ai.next();
                bi.next();
                if ca == cb {
                    continue;
                }
                let ord = ca.to_lowercase().cmp(cb.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["file10.txt", "File2.txt", "file1.txt", "a", "B", "file02.txt"];
        v.sort_by(|a, b| natural_cmp_ci(a, b));
        assert_eq!(v, ["a", "B", "file1.txt", "File2.txt", "file02.txt", "file10.txt"]);
        assert_eq!(natural_cmp_ci("abc", "ABC"), Ordering::Greater);
        assert_eq!(natural_cmp_ci("x9", "x10"), Ordering::Less);
        assert_eq!(natural_cmp_ci("x", "x1"), Ordering::Less);
    }
}
