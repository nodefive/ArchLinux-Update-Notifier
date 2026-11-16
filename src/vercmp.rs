//! Port of libalpm's `alpm_pkg_vercmp` / `rpmvercmp`, used to compare local
//! foreign package versions with AUR RPC results without spawning `vercmp`
//! once per package.

use std::cmp::Ordering;

/// Compare two full package versions (`[epoch:]version[-release]`).
pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (e1, v1, r1) = parse_evr(a);
    let (e2, v2, r2) = parse_evr(b);

    let mut ret = rpmvercmp(e1, e2);
    if ret == Ordering::Equal {
        ret = rpmvercmp(v1, v2);
        // Release is only compared when both sides have one, as in libalpm.
        if ret == Ordering::Equal {
            if let (Some(r1), Some(r2)) = (r1, r2) {
                ret = rpmvercmp(r1, r2);
            }
        }
    }
    ret
}

/// Split into (epoch, version, release). Mirrors libalpm's parseEVR: the
/// epoch is a leading run of digits followed by ':', the release is whatever
/// follows the last '-' after the epoch.
fn parse_evr(evr: &str) -> (&str, &str, Option<&str>) {
    let digits = evr.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, rest) = if evr.as_bytes().get(digits) == Some(&b':') {
        let e = &evr[..digits];
        (if e.is_empty() { "0" } else { e }, &evr[digits + 1..])
    } else {
        ("0", evr)
    };
    // libalpm searches for '-' starting after the digit run, which for the
    // no-epoch case is still inside `rest`; rfind over `rest` is equivalent
    // because a '-' cannot occur inside the leading digit run.
    match rest.rfind('-') {
        Some(i) => (epoch, &rest[..i], Some(&rest[i + 1..])),
        None => (epoch, rest, None),
    }
}

/// rpmvercmp from libalpm (lib/libalpm/version.c), operating on bytes.
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let s1 = a.as_bytes();
    let s2 = b.as_bytes();
    let (mut one, mut two) = (0usize, 0usize);

    while one < s1.len() && two < s2.len() {
        let seg1_start_sep = one;
        let seg2_start_sep = two;
        while one < s1.len() && !s1[one].is_ascii_alphanumeric() {
            one += 1;
        }
        while two < s2.len() && !s2[two].is_ascii_alphanumeric() {
            two += 1;
        }
        if one >= s1.len() || two >= s2.len() {
            break;
        }
        // Different separator lengths decide the comparison.
        let sep1 = one - seg1_start_sep;
        let sep2 = two - seg2_start_sep;
        if sep1 != sep2 {
            return if sep1 < sep2 { Ordering::Less } else { Ordering::Greater };
        }

        let (mut p1, mut p2) = (one, two);
        let isnum = s1[p1].is_ascii_digit();
        if isnum {
            while p1 < s1.len() && s1[p1].is_ascii_digit() {
                p1 += 1;
            }
            while p2 < s2.len() && s2[p2].is_ascii_digit() {
                p2 += 1;
            }
        } else {
            while p1 < s1.len() && s1[p1].is_ascii_alphabetic() {
                p1 += 1;
            }
            while p2 < s2.len() && s2[p2].is_ascii_alphabetic() {
                p2 += 1;
            }
        }

        // Segment types differ: numeric beats alpha.
        if p2 == two {
            return if isnum { Ordering::Greater } else { Ordering::Less };
        }

        let mut seg1 = &s1[one..p1];
        let mut seg2 = &s2[two..p2];
        if isnum {
            while seg1.first() == Some(&b'0') {
                seg1 = &seg1[1..];
            }
            while seg2.first() == Some(&b'0') {
                seg2 = &seg2[1..];
            }
            match seg1.len().cmp(&seg2.len()) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        match seg1.cmp(seg2) {
            Ordering::Equal => {}
            other => return other,
        }
        one = p1;
        two = p2;
    }

    let rest1 = &s1[one.min(s1.len())..];
    let rest2 = &s2[two.min(s2.len())..];
    if rest1.is_empty() && rest2.is_empty() {
        return Ordering::Equal;
    }
    // "The final showdown": a remaining alpha segment never beats an empty
    // string (1.0 > 1.0alpha), but anything else remaining wins.
    let one_alpha = rest1.first().is_some_and(u8::is_ascii_alphabetic);
    let two_alpha = rest2.first().is_some_and(u8::is_ascii_alphabetic);
    if (rest1.is_empty() && !two_alpha) || one_alpha {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    /// Cases taken from pacman's test/util/vercmptest.sh.
    const CASES: &[(&str, &str, Ordering)] = &[
        ("1.5.0", "1.5.0", Equal),
        ("1.5.1", "1.5.0", Greater),
        ("1.5.1", "1.5", Greater),
        ("1.5.0-1", "1.5.0-1", Equal),
        ("1.5.0-1", "1.5.0-2", Less),
        ("1.5.0-1", "1.5.1-1", Less),
        ("1.5.0-2", "1.5.1-1", Less),
        ("1.5-1", "1.5", Equal),
        ("1.5.0-1", "1.5.0", Equal),
        ("1.1-1", "1.1", Equal),
        ("1.0-1", "1.1", Less),
        ("1.1-1", "1.0", Greater),
        ("1.5b-1", "1.5-1", Less),
        ("1.5b", "1.5", Less),
        ("1.5b-1", "1.5", Less),
        ("1.5b", "1.5.1", Less),
        ("1.0a", "1.0alpha", Less),
        ("1.0alpha", "1.0b", Less),
        ("1.0b", "1.0beta", Less),
        ("1.0beta", "1.0rc", Less),
        ("1.0rc", "1.0", Less),
        ("1.5.a", "1.5", Greater),
        ("1.5.b", "1.5.a", Greater),
        ("1.5.1", "1.5.b", Greater),
        ("1.5.b-1", "1.5.b", Equal),
        ("1.5-1", "1.5.b", Less),
        ("2.0", "2_0", Equal),
        ("2.0_a", "2_0.a", Equal),
        ("2.0a", "2.0.a", Less),
        ("2___a", "2_a", Greater),
        ("0:1.0", "0:1.0", Equal),
        ("0:1.0", "0:1.1", Less),
        ("1:1.0", "0:1.0", Greater),
        ("1:1.0", "0:1.1", Greater),
        ("1:1.0", "2:1.1", Less),
        ("0:1.0", "1.0", Equal),
        ("0:1.0", "1.1", Less),
        ("0:1.1", "1.0", Greater),
        ("1:1.0", "1.0", Greater),
        ("1.0", "1:1.0", Less),
        ("1.0-1", "1.0-1", Equal),
        ("1.0-1", "1.0-2", Less),
        ("1.0-2", "1.0-1", Greater),
        ("1.0", "1.0.0", Less),
        ("1.0.0", "1.0", Greater),
        ("1.0", "1.0a", Greater),
        ("1.2.3", "1.2.3a", Greater),
        ("1.2.3.", "1.2.3", Greater),
        ("r123.abcdef-1", "r124.abcdef-1", Less),
    ];

    #[test]
    fn pacman_test_vectors() {
        for (a, b, want) in CASES {
            assert_eq!(vercmp(a, b), *want, "vercmp({a}, {b})");
            assert_eq!(vercmp(b, a), want.reverse(), "vercmp({b}, {a})");
        }
    }

    /// Cross-check against the real `vercmp` binary when available.
    #[test]
    fn matches_system_vercmp() {
        let Ok(probe) = std::process::Command::new("vercmp").args(["1", "2"]).output() else {
            eprintln!("vercmp not installed, skipping");
            return;
        };
        if !probe.status.success() {
            return;
        }
        for (a, b, _) in CASES {
            let out = std::process::Command::new("vercmp")
                .args([a, b])
                .output()
                .expect("vercmp ran once already");
            let n: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(99);
            assert_eq!(vercmp(a, b) as i32, n.signum(), "system vercmp({a}, {b})");
        }
    }
}
