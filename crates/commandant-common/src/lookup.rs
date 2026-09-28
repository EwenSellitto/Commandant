//! Finding things by exact key or unambiguous prefix, as users type ids.

#[derive(Debug, PartialEq, Eq)]
pub enum Match<T> {
    One(T),
    Ambiguous,
    None,
}

/// Returns the item whose key equals `needle`, else the only item whose key
/// starts with it.
pub fn find<T>(
    items: impl IntoIterator<Item = T>,
    needle: &str,
    key: impl Fn(&T) -> &str,
) -> Match<T> {
    if needle.is_empty() {
        return Match::None;
    }
    let mut prefixed = Vec::new();
    for item in items {
        if key(&item) == needle {
            return Match::One(item);
        }
        if key(&item).starts_with(needle) {
            prefixed.push(item);
        }
    }
    match prefixed.len() {
        0 => Match::None,
        1 => Match::One(prefixed.remove(0)),
        _ => Match::Ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_beats_prefix() {
        let ids = ["abc", "abd", "ab"];
        assert_eq!(find(ids, "ab", |s| s), Match::One("ab"));
        assert_eq!(find(ids, "abc", |s| s), Match::One("abc"));
        assert_eq!(find(["abc", "abd"], "ab", |s| s), Match::Ambiguous);
        assert_eq!(find(ids, "x", |s| s), Match::None);
        assert_eq!(find(ids, "", |s| s), Match::None);
    }
}
