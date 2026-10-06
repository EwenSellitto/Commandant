//! Finding things by exact key or unambiguous prefix, as users type ids.

#[derive(Debug, PartialEq, Eq)]
pub enum Match<T> {
    One(T),
    Ambiguous,
    None,
}

/// Returns the item named `needle`, else as [`find`] does by id: things
/// users name, like nodes, can also be given by id or id prefix.
pub fn find_named<T>(
    items: Vec<T>,
    needle: &str,
    name: impl Fn(&T) -> &str,
    id: impl Fn(&T) -> &str,
) -> Match<T> {
    match items.iter().position(|item| name(item) == needle) {
        Some(at) => Match::One(items.into_iter().nth(at).expect("just found")),
        None => find(items, needle, id),
    }
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

    #[test]
    fn a_name_beats_an_id() {
        let nodes = [("box", "b0c1"), ("b0", "e9f2"), ("lab", "b0d3")];
        let named = |needle| find_named(nodes.to_vec(), needle, |n| n.0, |n| n.1);
        assert_eq!(named("b0"), Match::One(("b0", "e9f2")));
        assert_eq!(named("lab"), Match::One(("lab", "b0d3")));
        assert_eq!(named("b0c"), Match::One(("box", "b0c1")));
        assert_eq!(named("b"), Match::Ambiguous);
    }
}
