//! Checks this project's row order against the one QuPath's own sort produces.
//!
//! `ProjectBrowser` orders its rows with `GeneralTools.smartStringSort`, which
//! is not a string sort: a name is split on `(\d+)`, the digit runs are parsed
//! to `BigDecimal` and compared by value, and the last tiebreak is the number
//! of parts. `parity/java/SmartStringSort.java` hands a list of names to that
//! function and records what comes back; this asserts the port puts the same
//! list in the same order.
//!
//! # What the first recording said
//!
//! Two things, and both are the kind that a reading of the Java gives the wrong
//! answer to:
//!
//! - **Java compares UTF-16 code units, so an emoji sorts before `\u{e000}`.**
//!   `String.compareTo` walks `char`s, and a character outside the basic plane
//!   is a *surrogate pair* in the `0xd800..0xe000` range — below every
//!   character from `\u{e000}` up. In UTF-8 bytes, which is what Rust's `str`
//!   comparison uses, `😀` sorts *after* both `\u{e000}` and `\u{ff21}`. The
//!   two orders disagree, and only for names carrying characters above the
//!   basic plane. See [`project::smart_string_compare`] for why the port
//!   follows Java here rather than the byte order.
//! - **`0` before `000` and `slide-007` before `slide-7`** — pairs the
//!   comparator calls *equal*, ordered by nothing but the input order that
//!   Java's stable sort left them in. A port whose comparator ranked them
//!   would put them in the same places for the wrong reason on this fixture
//!   and in different places on another one, so the test asserts the equality
//!   directly as well as the order.

use qupath_viewer::project;

const FIXTURE: &str = include_str!("../../../parity/fixtures/SmartStringSort.json");

/// The names in the fixture's `"<key>": [ ... ]` array, in the order it lists
/// them.
///
/// Scraped rather than parsed, as the other fixtures in this directory are:
/// `qupath_viewer::json` is crate-private, and the shape here is a flat array
/// of one string per line. The one thing a scrape must not skip is the `\u`
/// escapes — `SmartStringSort.java` writes every non-ASCII character as one,
/// and the names that carry them are exactly the ones this test exists for.
fn names(key: &str) -> Vec<String> {
    let heading = format!("\"{key}\": [");
    let at = FIXTURE.find(&heading).unwrap_or_else(|| panic!("a `{key}` array"));
    let rest = &FIXTURE[at + heading.len()..];
    let end = rest.find(']').expect("the array's end");
    rest[..end]
        .lines()
        .map(str::trim)
        .map(|line| line.trim_end_matches(','))
        .filter(|line| !line.is_empty())
        .map(unescape)
        .collect()
}

/// One JSON string literal, escapes and all, as the text it stands for.
///
/// Only the escapes `SmartStringSort.java` writes: `\"`, `\\`, and `\uXXXX`
/// including the surrogate pair a character outside the basic plane is written
/// as. A pair has to be recombined rather than decoded halfway — a lone
/// surrogate is not a `char`, and the emoji in the fixture is the reason the
/// UTF-16 ordering is worth a test at all.
fn unescape(literal: &str) -> String {
    let inner = literal
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or_else(|| panic!("a JSON string, got {literal}"));
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().expect("an escape") {
            'u' => {
                let first = hex(&mut chars);
                let scalar = if (0xd800..0xdc00).contains(&first) {
                    assert_eq!(chars.next(), Some('\\'), "a high surrogate's partner");
                    assert_eq!(chars.next(), Some('u'), "a high surrogate's partner");
                    let second = hex(&mut chars);
                    0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                } else {
                    first
                };
                out.push(char::from_u32(scalar).expect("a scalar value"));
            }
            other => out.push(other),
        }
    }
    out
}

/// The four hex digits after a `\u`.
fn hex(chars: &mut std::str::Chars<'_>) -> u32 {
    let digits: String = chars.take(4).collect();
    u32::from_str_radix(&digits, 16).expect("four hex digits")
}

/// Sorting QuPath's input with this comparator gives QuPath's output.
#[test]
fn matches_qupaths_smart_string_sort() {
    let input = names("input");
    let expected = names("sorted");
    assert_eq!(input.len(), expected.len(), "the fixture lists the same names twice");

    // `sort_by` and not `sort_unstable_by`, for the reason the doc comment on
    // the test module gives: the fixture holds pairs the comparator calls
    // equal, and Java's `sorted()` is stable, so only a stable sort here can
    // reproduce the order it left them in.
    let mut sorted = input.clone();
    sorted.sort_by(|one, two| project::smart_string_compare(one, two));

    assert_eq!(sorted, expected, "\n  ours: {sorted:?}\n  theirs: {expected:?}");
}

/// The recorded order really is non-decreasing under this comparator.
///
/// The test above would pass on a comparator that happened to agree with the
/// recorded order after a sort while disagreeing about individual pairs — the
/// sort can only ever produce *some* total order. This walks the recorded list
/// and asserts each neighbour is `<=` the next, which is the property
/// `SmartStringSort.java` asserts on its own side before printing.
#[test]
fn the_recorded_order_is_non_decreasing() {
    let sorted = names("sorted");
    for pair in sorted.windows(2) {
        let (one, two) = (&pair[0], &pair[1]);
        assert!(
            project::smart_string_compare(one, two) != std::cmp::Ordering::Greater,
            "{one:?} sorts after {two:?}, and QuPath put it before"
        );
    }
}

/// The pairs QuPath's comparator calls equal, asserted as equal here.
///
/// These are what the fixture's order cannot pin down: a stable sort leaves
/// equal elements in their input order, so a comparator that ranked them would
/// still reproduce the recorded list. Both come from `BigDecimal.compareTo`
/// ignoring scale — `007` and `7` are the same number, and the tie then falls
/// through to a part count that is also the same.
#[test]
fn leading_zeros_do_not_order_a_name() {
    use std::cmp::Ordering::Equal;
    assert_eq!(project::smart_string_compare("0", "000"), Equal);
    assert_eq!(project::smart_string_compare("slide-007", "slide-7"), Equal);
    assert_eq!(project::smart_string_compare("00a", "0a"), Equal);
}
