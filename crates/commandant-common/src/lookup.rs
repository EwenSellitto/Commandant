//! Finding things by exact key or unambiguous prefix, as users type ids.

/// What a lookup by key or prefix found.
#[derive(Debug, Eq, PartialEq)]
pub enum Match<T> {
    /// Exactly one item, or the one whose key is the whole needle.
    One(T),
    /// The needle fits several items.
    Ambiguous,
    /// Nothing fits.
    None,
}

/// Returns the item named `needle`, else as [`find`] does by id: things
/// users name, like nodes, can also be given by id or id prefix. A name that
/// is also the start of another item's id is ambiguous, so nobody can take
/// over an id by picking it as a name.
pub fn find_named<T>(
    mut items: Vec<T>,
    needle: &str,
    name: impl Fn(&T) -> &str,
    id: impl Fn(&T) -> &str,
) -> Match<T> {
    let Some(at) = items.iter().position(|item| name(item) == needle) else {
        return find(items, needle, id);
    };
    let mut others = items.iter().enumerate().filter(|(i, _)| *i != at);
    if others.any(|(_, item)| id(item).starts_with(needle)) {
        return Match::Ambiguous;
    }
    Match::One(items.swap_remove(at))
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
    fn names_and_ids_both_find_nodes() {
        let nodes = [
            ("box", "b0c1"),
            ("e9f2", "e9f2"),
            ("c7", "c7a0"),
            ("lab", "c7d3"),
        ];
        let named = |needle| find_named(nodes.to_vec(), needle, |(name, _)| *name, |(_, id)| *id);
        assert_eq!(named("box"), Match::One(("box", "b0c1")));
        assert_eq!(named("b0c"), Match::One(("box", "b0c1")));
        // A node may be named its own id.
        assert_eq!(named("e9f2"), Match::One(("e9f2", "e9f2")));
        assert_eq!(named("c7"), Match::Ambiguous);
        assert_eq!(named("x"), Match::None);
    }

    #[test]
    fn a_name_that_starts_another_nodes_id_is_ambiguous() {
        let nodes = [
            ("box", "b0c1"),
            ("b0", "e9f2"),
            ("lab", "a1d3"),
            ("a1d3", "f00d"),
        ];
        let named = |needle| find_named(nodes.to_vec(), needle, |(name, _)| *name, |(_, id)| *id);
        assert_eq!(named("b0"), Match::Ambiguous);
        assert_eq!(named("a1d3"), Match::Ambiguous);
        assert_eq!(named("e9"), Match::One(("b0", "e9f2")));
    }
}
