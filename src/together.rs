//! Songs that play together (`SongStore::together`) as shuffling plays
//! them: each set as one song, from the first of it on, in order.

/// A row's set, if it's in one: the set's ID, and where in it it is.
pub type Member<'a> = Option<(&'a str, usize)>;

/// The row that goes on with `current`'s set, of `rows` (those that can
/// play and haven't yet): the one that comes next in it.
pub fn next_in_set(members: &[Member], rows: &[usize], current: usize) -> Option<usize> {
    let (set, at) = members.get(current).copied().flatten()?;
    rows.iter().copied().filter(|&i| matches!(members[i], Some((s, n)) if s == set && n > at)).min_by_key(|&i| members[i].map(|(_, n)| n))
}

/// The rows of `rows` shuffling picks from: each set once, by the first
/// of it there.
pub fn firsts(members: &[Member], rows: &[usize]) -> Vec<usize> {
    let first = |i: usize| match members[i] {
        None => true,
        Some((set, at)) => !rows.iter().any(|&j| matches!(members[j], Some((s, n)) if s == set && (n, j) < (at, i))),
    };
    rows.iter().copied().filter(|&i| first(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Rows: a, then x2, b, x0, x1 (the set x, its songs out of order).
    const MEMBERS: [Member; 5] = [None, Some(("x", 2)), None, Some(("x", 0)), Some(("x", 1))];

    #[test]
    fn a_set_counts_once_from_its_first() {
        assert_eq!(firsts(&MEMBERS, &[0, 1, 2, 3, 4]), [0, 2, 3]);
        assert_eq!(firsts(&MEMBERS, &[0, 1, 4]), [0, 4], "what's left of it, from the first of that");
    }

    #[test]
    fn a_set_goes_on_in_order() {
        assert_eq!(next_in_set(&MEMBERS, &[0, 1, 2, 4], 3), Some(4));
        assert_eq!(next_in_set(&MEMBERS, &[0, 1, 2], 3), Some(1), "past one that can't play");
        assert_eq!(next_in_set(&MEMBERS, &[0, 2, 3, 4], 1), None, "the last of it");
        assert_eq!(next_in_set(&MEMBERS, &[1, 2], 0), None, "in none");
    }
}
