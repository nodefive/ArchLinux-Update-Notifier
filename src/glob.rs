//! Minimal glob matching (`*` and `?`) for the ignore list.

/// Match `name` against `pattern`, where `*` matches any run of characters
/// and `?` matches exactly one. Iterative with single backtrack point, so it
/// is linear in practice and cannot blow up on patterns like `a*a*a*b`.
pub fn matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_ni = 0usize;

    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_ni = ni;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub fn is_ignored(patterns: &[String], name: &str) -> bool {
    patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|p| matches(p, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_wildcards() {
        assert!(matches("linux", "linux"));
        assert!(!matches("linux", "linux-headers"));
        assert!(matches("linux*", "linux-headers"));
        assert!(matches("*-git", "neovim-git"));
        assert!(matches("lib?2", "lib32"));
        assert!(!matches("lib?2", "lib322"));
        assert!(matches("*", ""));
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(!matches("a*b*c", "axxbyy"));
    }

    #[test]
    fn ignore_list_skips_blank_entries() {
        let pats = vec!["  ".to_string(), "nvidia*".to_string()];
        assert!(is_ignored(&pats, "nvidia-utils"));
        assert!(!is_ignored(&pats, "mesa"));
    }
}
