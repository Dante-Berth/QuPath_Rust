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
///
/// # Why the array's end is not `find(']')`
///
/// Because a **name** may contain `]`, and the first version of this did
/// exactly that: `rest.find(']')` cut the array at the first bracket anywhere
/// in the remaining text. None of today's thirty-three names has one, so it
/// passed — and the failure it was hiding is the quiet kind. Both arrays are
/// scraped the same way, so both would be truncated at the same name, and
/// `matches_qupaths_smart_string_sort` would go on comparing two equally short
/// lists and reporting success. A test that stops covering the case somebody
/// just added a name for is worse than one that fails.
///
/// So the end is found by walking the array and tracking whether the cursor is
/// inside a string literal, which is the smallest amount of JSON that has to be
/// understood to know what a `]` means. `\\` skips the character after it, so
/// an escaped quote does not end a literal.
fn names(key: &str) -> Vec<String> {
    names_in(FIXTURE, key)
}

/// The same, over any text, so the scan itself can be tested on the case the
/// fixture does not carry — see `an_array_ends_at_its_own_bracket`.
fn names_in(text: &str, key: &str) -> Vec<String> {
    let heading = format!("\"{key}\": [");
    let at = text
        .find(&heading)
        .unwrap_or_else(|| panic!("a `{key}` array"));
    let rest = &text[at + heading.len()..];

    let mut end = None;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            ']' if !in_string => {
                end = Some(offset);
                break;
            }
            _ => {}
        }
    }
    let end = end.unwrap_or_else(|| panic!("the `{key}` array is never closed"));

    rest[..end]
        .lines()
        .map(str::trim)
        // One trailing separator, not every one: `trim_end_matches` is plural
        // and would eat a comma that is part of the last name's own text.
        .map(|line| line.strip_suffix(',').unwrap_or(line))
        // Blank *lines*, which the array's own layout has around it. The empty
        // *name* survives this, because after trimming it is `""` — two quote
        // characters — and not an empty line. That distinction is the whole
        // reason the fixture carries an empty name.
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

/// How many names `SmartStringSort.java` records, in each of its two arrays.
///
/// A literal, and it is the thing that makes every other assertion here mean
/// something. The scrape reads the fixture rather than parsing it, so the
/// failure it is prone to is reading *fewer* names than the file holds — and
/// two arrays scraped the same way truncate at the same place, which leaves
/// `matches_qupaths_smart_string_sort` comparing two short lists and passing.
/// Pinning the count is what turns that into a failure.
///
/// Raise it when a name is added to the recorder, which is also the moment to
/// re-run `parity/record.sh`.
const RECORDED: usize = 33;

/// Sorting QuPath's input with this comparator gives QuPath's output.
#[test]
fn matches_qupaths_smart_string_sort() {
    let input = names("input");
    let expected = names("sorted");
    assert_eq!(
        input.len(),
        expected.len(),
        "the fixture lists the same names twice"
    );
    assert_eq!(
        input.len(),
        RECORDED,
        "the scrape did not read the whole fixture"
    );

    // `sort_by` and not `sort_unstable_by`, for the reason the doc comment on
    // the test module gives: the fixture holds pairs the comparator calls
    // equal, and Java's `sorted()` is stable, so only a stable sort here can
    // reproduce the order it left them in.
    let mut sorted = input.clone();
    sorted.sort_by(|one, two| project::smart_string_compare(one, two));

    assert_eq!(
        sorted, expected,
        "\n  ours: {sorted:?}\n  theirs: {expected:?}"
    );
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

/// A `]` inside a name does not end the array.
///
/// The case the fixture cannot make, because none of its thirty-three names
/// has a bracket in it — which is exactly why the first version of `names_in`
/// cut the array at `find(']')` and passed anyway. Both arrays truncate
/// identically, so the comparison between them survives and stops covering
/// anything.
///
/// The escaped quote is here for the other half of the same scan: a `\"`
/// inside a literal must not be read as its end, or everything after it is
/// treated as being outside a string and the next `]` ends the array early.
#[test]
fn an_array_ends_at_its_own_bracket() {
    let text = concat!(
        "{\n  \"input\": [\n",
        "    \"a]b\",\n",
        "    \"quote \\\" and ] together\",\n",
        "    \"\",\n",
        "    \"z\"\n",
        "  ],\n  \"after\": []\n}\n",
    );
    assert_eq!(
        names_in(text, "input"),
        ["a]b", "quote \" and ] together", "", "z"],
    );
}
