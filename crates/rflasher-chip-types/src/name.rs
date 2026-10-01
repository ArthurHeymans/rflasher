//! Chip name patterns
//!
//! Database entries are named with the same small pattern language flashprog
//! uses, so that one entry can stand for a family of parts that behave alike:
//!
//! - `.` matches any single character (`W25Q64JV-.Q`);
//! - `A/B` matches either alternative (`MX25L1605D/MX25L1608D`);
//! - `(X)` is optional (`GD25Q64(B)` is `GD25Q64` or `GD25Q64B`), and
//!   `(X/Y)` is a required choice between `X` and `Y`
//!   (`MX25L4005(A/C)`); an empty first alternative, `(/X)`, makes the
//!   choice optional.
//!
//! Matching ignores ASCII case, so users can type the part number the way it
//! is printed on the chip.

/// Position of the first `needle` in `s` that is not inside parentheses
fn find_top_level(s: &[u8], needle: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ if b == needle && depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Position of the `)` closing the `(` at `s[0]`
fn closing_paren(s: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether one alternative (no top-level `/`) matches all of `name`
fn alternative_matches(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some(b'(') => {
            // A group without a matching parenthesis is taken literally.
            let Some(close) = closing_paren(pattern) else {
                return name.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'('))
                    && alternative_matches(&pattern[1..], &name[1..]);
            };
            let (inner, rest) = (&pattern[1..close], &pattern[close + 1..]);
            let first_end = find_top_level(inner, b'/');
            // `(X)` is optional; `(X/Y)` is a required choice unless the
            // first alternative is empty.
            let optional = first_end.is_none_or(|end| end == 0);

            let mut choices = inner;
            loop {
                let (choice, remaining) = match find_top_level(choices, b'/') {
                    Some(end) => (&choices[..end], Some(&choices[end + 1..])),
                    None => (choices, None),
                };
                // The choice consumes some prefix of `name`; the rest of the
                // pattern must match what is left.
                if (0..=name.len()).any(|split| {
                    alternative_matches(choice, &name[..split])
                        && alternative_matches(rest, &name[split..])
                }) {
                    return true;
                }
                match remaining {
                    Some(r) => choices = r,
                    None => break,
                }
            }
            optional && alternative_matches(rest, name)
        }
        Some(b'.') => !name.is_empty() && alternative_matches(&pattern[1..], &name[1..]),
        Some(c) => {
            name.first().is_some_and(|n| n.eq_ignore_ascii_case(c))
                && alternative_matches(&pattern[1..], &name[1..])
        }
    }
}

/// Whether the database entry name `pattern` matches the part number `wanted`
///
/// See the [module documentation](self) for the pattern language.
#[must_use]
pub fn name_matches(pattern: &str, wanted: &str) -> bool {
    let wanted = wanted.as_bytes();
    let mut rest = pattern.as_bytes();
    loop {
        let (alternative, remaining) = match find_top_level(rest, b'/') {
            Some(end) => (&rest[..end], Some(&rest[end + 1..])),
            None => (rest, None),
        };
        if alternative_matches(alternative, wanted) {
            return true;
        }
        match remaining {
            Some(r) => rest = r,
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::name_matches;

    #[test]
    fn literal_names_match_ignoring_case() {
        assert!(name_matches("W25Q128FV", "W25Q128FV"));
        assert!(name_matches("W25Q128FV", "w25q128fv"));
        assert!(!name_matches("W25Q128FV", "W25Q128JV"));
        assert!(!name_matches("W25Q128FV", "W25Q128F"));
        assert!(!name_matches("W25Q128FV", "W25Q128FVX"));
        assert!(!name_matches("W25Q128FV", ""));
    }

    #[test]
    fn dot_matches_exactly_one_character() {
        assert!(name_matches("W25Q128.V", "W25Q128FV"));
        assert!(name_matches("W25Q128.V", "W25Q128JV"));
        assert!(name_matches("W25Q64JV-.Q", "W25Q64JV-IQ"));
        assert!(!name_matches("W25Q128.V", "W25Q128V"));
        assert!(!name_matches("W25Q128.V", "W25Q128FFV"));
        assert!(name_matches("N25Q00A..3G", "N25Q00A213G"));
    }

    #[test]
    fn slash_separates_alternatives() {
        let pattern = "W25Q64BV/W25Q64CV/W25Q64FV";
        for name in ["W25Q64BV", "W25Q64CV", "w25q64fv"] {
            assert!(name_matches(pattern, name), "{name}");
        }
        assert!(!name_matches(pattern, "W25Q64DV"));
        assert!(!name_matches(pattern, "W25Q64BV/W25Q64CV"));
    }

    #[test]
    fn single_alternative_group_is_optional() {
        assert!(name_matches("GD25Q64(B)", "GD25Q64"));
        assert!(name_matches("GD25Q64(B)", "GD25Q64B"));
        assert!(!name_matches("GD25Q64(B)", "GD25Q64C"));
        assert!(name_matches("Am29F002(N)BT", "Am29F002BT"));
        assert!(name_matches("Am29F002(N)BT", "Am29F002NBT"));
    }

    #[test]
    fn group_with_alternatives_is_a_required_choice() {
        let pattern = "MX25L4005(A/C)";
        assert!(name_matches(pattern, "MX25L4005A"));
        assert!(name_matches(pattern, "MX25L4005C"));
        assert!(!name_matches(pattern, "MX25L4005"));
        assert!(!name_matches(pattern, "MX25L4005B"));
        // An empty first alternative makes it optional.
        assert!(name_matches("MX25L4005(/A/C)", "MX25L4005"));
        assert!(name_matches("MX25L4005(/A/C)", "MX25L4005C"));
    }

    #[test]
    fn groups_combine_with_alternatives_and_wildcards() {
        let pattern = "MX25L4005(A/C)/MX25L4006E";
        assert!(name_matches(pattern, "MX25L4005A"));
        assert!(name_matches(pattern, "MX25L4006E"));
        assert!(!name_matches(pattern, "MX25L4005"));
        assert!(name_matches("XT25F128F/XT25BF128F", "XT25BF128F"));
        assert!(name_matches("S25FL128P......0", "S25FL128PABMFI10"));
    }

    #[test]
    fn unbalanced_parentheses_do_not_panic() {
        assert!(name_matches("ABC(", "ABC("));
        assert!(!name_matches("ABC(", "ABC"));
        assert!(!name_matches(")", "x"));
        assert!(!name_matches("", "x"));
        assert!(name_matches("", ""));
    }
}
