//! Reading JSON, for the one file this crate has to read back.
//!
//! Every other JSON in this workspace is *written*: [`crate::geojson`] and
//! [`crate::project`] build text for QuPath to read, and a writer needs no
//! parser. `project.qpproj` is the first file that goes the other way — a
//! project browser opens a project someone else's application wrote, and
//! `DefaultProject.writeProject` puts it through Gson with pretty-printing on.
//!
//! # Why this is a parser and not a search for a key
//!
//! `parity/fixtures` are read in tests by looking for `"key": ` and taking what
//! follows, and that is fine for a fixture whose shape is fixed by the recorder
//! that wrote it. It is not fine here. A `serverBuilder` QuPath wrote carries a
//! `metadata` block, and that block has a `uri` of its own inside
//! `pixelCalibration`-adjacent structures, `levels` arrays and `channels`
//! objects. The first `"uri"` in an entry is not reliably the entry's, and a
//! reader that takes it is one that opens the wrong slide — silently, because
//! both are strings and both look like paths.
//!
//! So this walks the text. It is small because the subset is small: the
//! grammar is all of JSON, and nothing here interprets it.
//!
//! # Numbers keep their text
//!
//! [`Value::Number`] holds the literal rather than an `f64`. Two of the fields
//! this exists to read are millisecond timestamps — `createTimestamp` is around
//! 1.7e12 — and while those are exact in an `f64`, `lastID` and `entryID` are
//! counters that have no reason to go through a float at all. Parsing the text
//! on demand means a `u64` field is read as a `u64`, which is what it is.

use std::fmt::Write as _;

/// A JSON value.
///
/// An object keeps its pairs in the order they were written, in a `Vec` rather
/// than a map: the objects here have a handful of fields, a linear scan beats
/// hashing at that size, and the order is what makes a re-serialisation
/// comparable against the file it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as the literal text that was written.
    Number(String),
    /// A string, with its escapes resolved.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, in file order.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value of one field, or `None` for anything that is not an object
    /// with that key.
    ///
    /// Takes the *first* match. Gson cannot write a duplicate key, and a file
    /// that has one is malformed in a way this does not try to adjudicate.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The string, or `None` if this is not one.
    ///
    /// Deliberately not lenient: a number where a string was expected is a file
    /// in a different format, and coercing it hides that.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// The number as a `u64`, or `None` if it is not one — including a number
    /// with a fraction or an exponent, which is not a count.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The fields, in the order the file lists them, or `None` if this is not
    /// an object.
    ///
    /// The order matters and is not incidental. Gson serialises a
    /// `LinkedHashMap`, which is what `DefaultProjectImageEntry` holds its
    /// metadata in, so the file's order is the order a user put the keys in —
    /// and a browser that showed them in some other order would be reordering
    /// something someone chose.
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(fields) => Some(fields),
            _ => None,
        }
    }

    /// The elements, or `None` if this is not an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// Parses one JSON document.
///
/// The whole text must be one value: trailing content is an error rather than
/// something to ignore, because a truncated file that happens to close its
/// first object would otherwise read as a short project.
///
/// # Errors
///
/// Returns a message naming the byte offset. The offset is what makes a
/// malformed project locatable — the alternative is "invalid JSON" about a file
/// of several thousand lines.
pub fn parse(text: &str) -> Result<Value, String> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let value = value(bytes, &mut at)?;
    space(bytes, &mut at);
    if at != bytes.len() {
        return Err(problem(bytes, at, "trailing text after the document"));
    }
    Ok(value)
}

/// A message naming where in the text the reader stopped.
///
/// The line and column as well as the offset, because a byte offset alone is
/// not something anyone can find in an editor.
fn problem(bytes: &[u8], at: usize, what: &str) -> String {
    let seen = &bytes[..at.min(bytes.len())];
    let line = 1 + seen.iter().filter(|byte| **byte == b'\n').count();
    let column = 1 + seen.len()
        - seen
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |n| n + 1);
    let mut out = String::new();
    let _ = write!(out, "{what}, at line {line} column {column} (byte {at})");
    out
}

/// Skips whitespace. JSON's is the four characters, and no comments.
fn space(bytes: &[u8], at: &mut usize) {
    while let Some(byte) = bytes.get(*at) {
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            *at += 1;
        } else {
            break;
        }
    }
}

/// One value, with leading whitespace skipped.
fn value(bytes: &[u8], at: &mut usize) -> Result<Value, String> {
    space(bytes, at);
    match bytes.get(*at) {
        None => Err(problem(
            bytes,
            *at,
            "a value was expected and the text ended",
        )),
        Some(b'{') => object(bytes, at),
        Some(b'[') => array(bytes, at),
        Some(b'"') => string(bytes, at).map(Value::String),
        Some(b't') => literal(bytes, at, "true", Value::Bool(true)),
        Some(b'f') => literal(bytes, at, "false", Value::Bool(false)),
        Some(b'n') => literal(bytes, at, "null", Value::Null),
        Some(_) => number(bytes, at),
    }
}

/// `true`, `false` or `null`, which are the only bare words JSON has.
fn literal(bytes: &[u8], at: &mut usize, word: &str, value: Value) -> Result<Value, String> {
    if bytes[*at..].starts_with(word.as_bytes()) {
        *at += word.len();
        Ok(value)
    } else {
        Err(problem(bytes, *at, &format!("`{word}` was expected")))
    }
}

/// A number, kept as the text it was written as — see this module's header.
///
/// The span is taken by the characters a JSON number may contain rather than by
/// validating the grammar, and then checked by parsing it as an `f64`. That
/// rejects `1.2.3` and `--1` for the same reason a grammar would, in one line
/// that cannot disagree with the one that reads the value back.
fn number(bytes: &[u8], at: &mut usize) -> Result<Value, String> {
    let start = *at;
    while let Some(byte) = bytes.get(*at) {
        if matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            *at += 1;
        } else {
            break;
        }
    }
    let text = std::str::from_utf8(&bytes[start..*at])
        .map_err(|_| problem(bytes, start, "a number was expected"))?;
    if text.is_empty() || text.parse::<f64>().is_err() {
        return Err(problem(bytes, start, "a value was expected"));
    }
    Ok(Value::Number(text.to_owned()))
}

/// A string, with its escapes resolved.
///
/// Everything between escapes is copied as a **span** rather than a byte at a
/// time, and that is correctness and not speed: a byte of a multi-byte
/// character pushed with `as char` becomes the Latin-1 character of the same
/// number, so `\u{b5}` — the micron sign this format compares by string
/// equality — would come back as two characters. The spans always begin and end
/// on a `"` or a `\`, both ASCII, so they are never cut through a character.
fn string(bytes: &[u8], at: &mut usize) -> Result<String, String> {
    if bytes.get(*at) != Some(&b'"') {
        return Err(problem(bytes, *at, "a string was expected"));
    }
    *at += 1;
    let mut out = String::new();
    let mut run = *at;
    loop {
        let byte = *bytes
            .get(*at)
            .ok_or_else(|| problem(bytes, *at, "a string was not closed"))?;
        match byte {
            b'"' => {
                span(bytes, run, *at, &mut out)?;
                *at += 1;
                return Ok(out);
            }
            b'\\' => {
                span(bytes, run, *at, &mut out)?;
                *at += 1;
                escape(bytes, at, &mut out)?;
                run = *at;
            }
            // A control character must be escaped in JSON, and Gson escapes
            // them. One arriving raw is a file this cannot claim to have read.
            0x00..=0x1f => return Err(problem(bytes, *at, "an unescaped control character")),
            _ => *at += 1,
        }
    }
}

/// Appends the text between two offsets, which is a run with no escape in it.
fn span(bytes: &[u8], from: usize, to: usize, out: &mut String) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    let text = std::str::from_utf8(&bytes[from..to])
        .map_err(|_| problem(bytes, from, "text that is not UTF-8"))?;
    out.push_str(text);
    Ok(())
}

/// One `\`-escape, already past the backslash.
fn escape(bytes: &[u8], at: &mut usize, out: &mut String) -> Result<(), String> {
    let byte = *bytes
        .get(*at)
        .ok_or_else(|| problem(bytes, *at, "an escape was not finished"))?;
    *at += 1;
    let resolved = match byte {
        b'"' => '"',
        b'\\' => '\\',
        b'/' => '/',
        b'b' => '\u{8}',
        b'f' => '\u{c}',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'u' => return unicode(bytes, at, out),
        _ => return Err(problem(bytes, *at - 1, "an escape this does not know")),
    };
    out.push(resolved);
    Ok(())
}

/// A `\uXXXX` escape, and the surrogate pair that follows one when the
/// character is past the basic plane.
///
/// Gson escapes little by default — it writes `µ` as itself — but it escapes
/// what it must, and a project named with an emoji is written as a pair of
/// surrogates. Decoding the first half alone yields a lone surrogate, which is
/// not a `char`, so the pair has to be recognised here or not at all.
fn unicode(bytes: &[u8], at: &mut usize, out: &mut String) -> Result<(), String> {
    let first = hex4(bytes, at)?;
    let scalar = if (0xd800..0xdc00).contains(&first) {
        // A high surrogate. Its partner must follow as its own `\u` escape.
        if bytes.get(*at) != Some(&b'\\') || bytes.get(*at + 1) != Some(&b'u') {
            return Err(problem(
                bytes,
                *at,
                "a high surrogate with no low surrogate",
            ));
        }
        *at += 2;
        let second = hex4(bytes, at)?;
        if !(0xdc00..0xe000).contains(&second) {
            return Err(problem(
                bytes,
                *at,
                "a high surrogate followed by a non-surrogate",
            ));
        }
        0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00)
    } else {
        first
    };
    let character = char::from_u32(scalar)
        .ok_or_else(|| problem(bytes, *at, "an escape naming no character"))?;
    out.push(character);
    Ok(())
}

/// The four hex digits of a `\u` escape.
fn hex4(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let digits = bytes
        .get(*at..*at + 4)
        .ok_or_else(|| problem(bytes, *at, "a `\\u` escape with fewer than four digits"))?;
    let mut scalar = 0u32;
    for digit in digits {
        let nibble = match digit {
            b'0'..=b'9' => u32::from(digit - b'0'),
            b'a'..=b'f' => u32::from(digit - b'a') + 10,
            b'A'..=b'F' => u32::from(digit - b'A') + 10,
            _ => return Err(problem(bytes, *at, "a `\\u` escape with a non-hex digit")),
        };
        scalar = scalar * 16 + nibble;
    }
    *at += 4;
    Ok(scalar)
}

/// An array, already at its `[`.
fn array(bytes: &[u8], at: &mut usize) -> Result<Value, String> {
    *at += 1;
    let mut items = Vec::new();
    space(bytes, at);
    if bytes.get(*at) == Some(&b']') {
        *at += 1;
        return Ok(Value::Array(items));
    }
    loop {
        items.push(value(bytes, at)?);
        space(bytes, at);
        match bytes.get(*at) {
            Some(b',') => *at += 1,
            Some(b']') => {
                *at += 1;
                return Ok(Value::Array(items));
            }
            _ => return Err(problem(bytes, *at, "a `,` or a `]` was expected")),
        }
    }
}

/// An object, already at its `{`.
fn object(bytes: &[u8], at: &mut usize) -> Result<Value, String> {
    *at += 1;
    let mut fields = Vec::new();
    space(bytes, at);
    if bytes.get(*at) == Some(&b'}') {
        *at += 1;
        return Ok(Value::Object(fields));
    }
    loop {
        space(bytes, at);
        let key = string(bytes, at)?;
        space(bytes, at);
        if bytes.get(*at) != Some(&b':') {
            return Err(problem(bytes, *at, "a `:` was expected"));
        }
        *at += 1;
        fields.push((key, value(bytes, at)?));
        space(bytes, at);
        match bytes.get(*at) {
            Some(b',') => *at += 1,
            Some(b'}') => {
                *at += 1;
                return Ok(Value::Object(fields));
            }
            _ => return Err(problem(bytes, *at, "a `,` or a `}` was expected")),
        }
    }
}

/// Writes a value the way Gson does with pretty-printing on.
///
/// # Why the spelling is copied and not merely valid
///
/// This exists to rewrite a project **without losing what was not
/// interpreted** — an entry's description, its metadata map, its tags, a stored
/// server metadata block. Re-serialising is what preserves those, and the check
/// that says the preservation is complete is that a file QuPath wrote comes
/// back byte for byte. That check is only available if the spelling matches, so
/// the spelling is copied: two-space indent, `": "` between a key and its
/// value, empty containers inline, and Gson's escapes.
///
/// The escapes are the part that had to be measured rather than assumed.
/// `GsonTools` never calls `disableHtmlEscaping`, so the five characters
/// `<`, `>`, `&`, `=` and `'` are written as lowercase `\u00xx` escapes, while
/// `/` is left alone and every character above the ASCII range is written as
/// itself. U+2028 and U+2029 are escaped despite being far above the control
/// range, because a raw one is a line terminator to a JavaScript parser and
/// Gson special-cases them.
///
/// There is **no trailing newline**, which is also Gson's.
///
/// A `null` is written where one was read. Gson drops nulls rather than writing
/// them — `serializeNulls` is off — so a file Gson wrote never has one, and the
/// two agree on every file this is used to rewrite. Keeping it is the choice
/// that loses nothing from a file Gson did not write.
pub fn write(value: &Value) -> String {
    let mut out = String::new();
    put(value, 0, &mut out);
    out
}

/// The two spaces Gson's `setPrettyPrinting` indents by.
const INDENT: &str = "  ";

fn put(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        // The literal that was read, so a number never goes through a float and
        // back. `1.0`, `1` and `1.0E30` are three different texts for values a
        // reader could not tell apart afterwards.
        Value::Number(text) => out.push_str(text),
        Value::String(text) => quote(text, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                put(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        Value::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (at, (key, field)) in fields.iter().enumerate() {
                if at > 0 {
                    out.push_str(",\n");
                }
                indent(depth + 1, out);
                quote(key, out);
                out.push_str(": ");
                put(field, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// One JSON string, escaped as Gson escapes it.
fn quote(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // Gson's HTML-safe set, which `GsonTools` leaves on. `/` is
            // deliberately not in it — every path in a project is full of them,
            // and Gson writes them as themselves.
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003d"),
            '\'' => out.push_str("\\u0027"),
            // Line and paragraph separator. Far above the control range and
            // escaped anyway, because a raw one terminates a line for a
            // JavaScript parser reading the same file.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            control if control < ' ' => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            // Everything else as itself, including every character above the
            // ASCII range: the micron sign, an accent and an emoji are all
            // written raw.
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_of_a_project_index_survives() {
        let value = parse(
            r#"{ "version": "0.6.0", "lastID": 2,
                 "images": [ { "entryID": 1, "imageName": "a" },
                             { "entryID": 2, "imageName": "b" } ] }"#,
        )
        .expect("valid JSON");
        assert_eq!(value.get("version").and_then(Value::as_str), Some("0.6.0"));
        assert_eq!(value.get("lastID").and_then(Value::as_u64), Some(2));
        let images = value
            .get("images")
            .and_then(Value::as_array)
            .expect("images");
        assert_eq!(images.len(), 2);
        assert_eq!(
            images[1].get("imageName").and_then(Value::as_str),
            Some("b")
        );
    }

    /// The reason this is a parser: the *first* `"uri"` in an entry QuPath
    /// wrote is not the entry's.
    ///
    /// A reader that searches for `"uri": ` takes whichever comes first in the
    /// text, and a `serverBuilder` with a metadata block has several. Both are
    /// strings and both look like paths, so the wrong one opens the wrong file
    /// and says nothing.
    #[test]
    fn a_nested_key_does_not_shadow_the_one_being_asked_for() {
        let value = parse(
            r#"{ "serverBuilder": { "metadata": { "uri": "file:///wrong" },
                                    "uri": "file:///right" } }"#,
        )
        .expect("valid JSON");
        let builder = value.get("serverBuilder").expect("a builder");
        assert_eq!(
            builder.get("uri").and_then(Value::as_str),
            Some("file:///right")
        );
        // And the outer object has no `uri` of its own, rather than borrowing
        // one from a child.
        assert_eq!(value.get("uri"), None);
    }

    #[test]
    fn escapes_come_back_as_the_characters_they_name() {
        let value = parse(r#""a\"b\\c\/d\ne\tf\u00b5g""#).expect("valid JSON");
        assert_eq!(value.as_str(), Some("a\"b\\c/d\ne\tf\u{b5}g"));
    }

    /// A multi-byte character copied a byte at a time becomes two characters.
    ///
    /// `µ` is not a decorative case: QuPath compares pixel-size units against
    /// `"\u{b5}m"` by string equality, so a reader that mangles it reads a
    /// calibration that matches nothing.
    #[test]
    fn a_character_outside_ascii_survives_unescaped() {
        let value = parse("\"0.2498 \u{b5}m — Ω\"").expect("valid JSON");
        assert_eq!(value.as_str(), Some("0.2498 \u{b5}m — Ω"));
        assert_eq!(
            value.as_str().map(str::chars).map(Iterator::count),
            Some(13)
        );
    }

    /// A character past the basic plane arrives as two escapes, and neither
    /// half is a character on its own.
    #[test]
    fn a_surrogate_pair_is_one_character() {
        let value = parse(r#""slide \ud83d\udd2c ready""#).expect("valid JSON");
        assert_eq!(value.as_str(), Some("slide 🔬 ready"));
        assert!(
            parse(r#""\ud83d""#).is_err(),
            "a lone high surrogate is not a character"
        );
        assert!(
            parse(r#""\ud83d\u0041""#).is_err(),
            "a high surrogate needs a low one"
        );
    }

    /// Numbers keep their text, so a counter is read as a counter.
    #[test]
    fn a_timestamp_is_read_as_an_integer_and_not_through_a_float() {
        let value = parse(r#"{"createTimestamp": 1700000000000, "big": 9007199254740993}"#)
            .expect("valid JSON");
        assert_eq!(
            value.get("createTimestamp").and_then(Value::as_u64),
            Some(1_700_000_000_000)
        );
        // One past 2^53, which is the first integer an `f64` cannot hold. A
        // parser that went through a float would answer 9007199254740992.
        assert_eq!(
            value.get("big").and_then(Value::as_u64),
            Some(9_007_199_254_740_993)
        );
    }

    /// A count is a count: a fraction is not one, and reading it as though it
    /// were rounds silently.
    #[test]
    fn a_fraction_is_not_a_count() {
        let value = parse(r#"{"a": 1.5, "b": -3, "c": 2e3}"#).expect("valid JSON");
        assert_eq!(value.get("a").and_then(Value::as_u64), None);
        assert_eq!(value.get("b").and_then(Value::as_u64), None);
        assert_eq!(value.get("c").and_then(Value::as_u64), None);
    }

    #[test]
    fn empty_containers_are_containers() {
        assert_eq!(parse("[]").expect("valid"), Value::Array(Vec::new()));
        assert_eq!(parse("{}").expect("valid"), Value::Object(Vec::new()));
        assert_eq!(
            parse(r#"{"images": []}"#)
                .expect("valid")
                .get("images")
                .and_then(Value::as_array),
            Some(&[][..])
        );
    }

    #[test]
    fn the_three_bare_words() {
        assert_eq!(parse("true").expect("valid"), Value::Bool(true));
        assert_eq!(parse("false").expect("valid"), Value::Bool(false));
        assert_eq!(parse("null").expect("valid"), Value::Null);
        assert!(parse("tru").is_err());
        assert!(parse("nil").is_err());
    }

    /// A file cut off partway must not read as a shorter file.
    ///
    /// The failure that matters is a project whose `images` array is truncated:
    /// a lenient reader gives back the entries it managed, and the browser then
    /// shows a project that has quietly lost slides.
    #[test]
    fn a_truncated_document_is_an_error_and_not_a_short_one() {
        for broken in [
            r#"{"images": [{"entryID": 1},"#,
            r#"{"images": [{"entryID": 1}"#,
            r#"{"version": "0.6.0""#,
            r#"{"version": "0.6"#,
            "",
        ] {
            assert!(parse(broken).is_err(), "read {broken:?} as valid");
        }
    }

    /// Two values in a file is not one value, even when the first one parses.
    #[test]
    fn trailing_text_is_an_error() {
        assert!(parse(r#"{"a": 1} {"b": 2}"#).is_err());
        assert!(parse("1 2").is_err());
        // Whitespace after the document is not trailing text.
        assert_eq!(
            parse(" {\"a\": 1} \n")
                .expect("valid")
                .get("a")
                .and_then(Value::as_u64),
            Some(1)
        );
    }

    /// The message has to name a place, or a malformed project of several
    /// thousand lines reports "invalid JSON" and nothing else.
    #[test]
    fn an_error_says_where_it_stopped() {
        let error = parse("{\n  \"a\": 1,\n  \"b\": ]\n}").expect_err("invalid");
        assert!(error.contains("line 3"), "{error}");
        assert!(error.contains("column 8"), "{error}");
    }

    /// Malformed input must not be read as something.
    #[test]
    fn what_is_rejected() {
        for broken in [
            "{a: 1}",      // an unquoted key
            "{\"a\" 1}",   // no colon
            "[1 2]",       // no comma
            "\"a\nb\"",    // a raw control character in a string
            "\"\\q\"",     // an escape that is not one
            "\"\\u00g1\"", // a non-hex digit
            "\"\\u00\"",   // fewer than four digits
            "1.2.3",       // not a number
            "--1",         // not a number either
            "{\"a\": }",   // a key with no value
        ] {
            assert!(parse(broken).is_err(), "read {broken:?} as valid");
        }
    }

    /// A string that is not one is `None` rather than a coerced value, because
    /// a coercion here is a project in a different format read as this one.
    #[test]
    fn accessors_do_not_coerce() {
        let value = parse(r#"{"n": 1, "s": "1", "a": [], "o": {}}"#).expect("valid");
        assert_eq!(value.get("n").and_then(Value::as_str), None);
        assert_eq!(value.get("s").and_then(Value::as_u64), None);
        assert_eq!(value.get("o").and_then(Value::as_array), None);
        assert_eq!(value.get("a").and_then(Value::as_str), None);
        // `get` on anything that is not an object is `None`, not a panic.
        assert_eq!(value.get("a").and_then(|array| array.get("0")), None);
    }

    /// The shape Gson writes, measured against a real one rather than assumed.
    ///
    /// Two-space indent, `": "` between a key and its value, empty containers
    /// inline, no trailing newline.
    #[test]
    fn the_layout_is_gsons() {
        let value =
            parse(r#"{"a": 1, "empty": {}, "none": [], "list": [1, {"b": "c"}]}"#).expect("valid");
        assert_eq!(
            write(&value),
            "{\n  \"a\": 1,\n  \"empty\": {},\n  \"none\": [],\n  \"list\": [\n    1,\n    \
             {\n      \"b\": \"c\"\n    }\n  ]\n}"
        );
    }

    /// `GsonTools` never calls `disableHtmlEscaping`, so five ordinary
    /// characters come out as lowercase `\u00xx` escapes — and `/` does not.
    ///
    /// Recorded from a real `GsonTools.getInstance(true)` rather than read off
    /// Gson's defaults: it is the difference between a rewritten project that
    /// matches the one it came from and one that differs on every path it
    /// contains.
    #[test]
    fn gsons_html_escapes_are_copied_exactly() {
        let value = Value::String("a<b>c&d=e'f\"g\\h/i".to_owned());
        assert_eq!(
            write(&value),
            "\"a\\u003cb\\u003ec\\u0026d\\u003de\\u0027f\\\"g\\\\h/i\"",
        );
    }

    #[test]
    fn control_characters_take_gsons_spellings() {
        assert_eq!(
            write(&Value::String("\t\n\r\u{8}\u{c}".to_owned())),
            r#""\t\n\r\b\f""#
        );
        assert_eq!(
            write(&Value::String("\u{1}\u{1f}".to_owned())),
            "\"\\u0001\\u001f\""
        );
        // U+007F and U+009F are above the escaped range and are written raw,
        // which is what Gson does with them.
        assert_eq!(
            write(&Value::String("\u{7f}\u{9f}".to_owned())),
            "\"\u{7f}\u{9f}\""
        );
    }

    /// Two characters far above the control range that Gson escapes anyway,
    /// because a raw one ends a line for a JavaScript parser.
    #[test]
    fn the_line_separators_are_escaped() {
        let value = Value::String("x\u{2028}y\u{2029}z".to_owned());
        assert_eq!(write(&value), "\"x\\u2028y\\u2029z\"");
    }

    /// Everything above ASCII is written as itself, so a pixel unit stays one
    /// character and a study named in Greek stays readable in the file.
    #[test]
    fn characters_outside_ascii_are_written_raw() {
        let value = Value::String("0.2498 \u{b5}m — études 🔬".to_owned());
        assert_eq!(write(&value), "\"0.2498 \u{b5}m — études 🔬\"");
    }

    /// A number is written as the text it was read as, so nothing goes through
    /// a float and back.
    #[test]
    fn numbers_keep_the_text_they_arrived_as() {
        let value = parse(r#"{"a": 1.0, "b": 1, "c": 1.0E30, "d": -0.0, "e": 9007199254740993}"#)
            .expect("valid");
        assert_eq!(
            write(&value),
            "{\n  \"a\": 1.0,\n  \"b\": 1,\n  \"c\": 1.0E30,\n  \"d\": -0.0,\n  \
             \"e\": 9007199254740993\n}"
        );
    }

    /// Whatever this writes, it must read back the same.
    ///
    /// The weaker half of the byte-exact check in `parity_project_export.rs`,
    /// and the half that covers the characters no fixture happens to hold.
    #[test]
    fn what_is_written_parses_back_to_what_it_was() {
        let value = parse(concat!(
            r#"{"name": "a<b>&c='d\"e\\f\tg", "unicode": "µm 🔬", "#,
            r#""n": [1.0, -2, 3.5E-7], "deep": {"empty": {}, "list": []}, "#,
            r#""flags": [true, false, null]}"#,
        ))
        .expect("valid");
        assert_eq!(parse(&write(&value)).expect("what was written"), value);
    }

    #[test]
    fn a_bare_value_is_a_document() {
        assert_eq!(write(&Value::Null), "null");
        assert_eq!(write(&Value::Bool(true)), "true");
        assert_eq!(write(&parse("[]").expect("valid")), "[]");
        assert_eq!(write(&parse("{}").expect("valid")), "{}");
    }

    /// Gson pretty-prints, so the reader has to be indifferent to layout.
    #[test]
    fn whitespace_is_not_structure() {
        let dense = parse(r#"{"images":[{"entryID":1}],"lastID":1}"#).expect("valid");
        let spread = parse(
            "{\n  \"images\" : [\r\n\t{\n\t  \"entryID\" : 1\n\t}\n  ] ,\n  \"lastID\" : 1\n}",
        )
        .expect("valid");
        assert_eq!(dense, spread);
    }

    /// Nesting deep enough to matter, since a `serverBuilder`'s metadata block
    /// is four levels down from the project.
    #[test]
    fn nesting_is_followed_to_the_bottom() {
        let value = parse(r#"{"a":{"b":{"c":{"d":[[{"e":"found"}]]}}}}"#).expect("valid");
        let leaf = value
            .get("a")
            .and_then(|value| value.get("b"))
            .and_then(|value| value.get("c"))
            .and_then(|value| value.get("d"))
            .and_then(Value::as_array)
            .and_then(<[Value]>::first)
            .and_then(Value::as_array)
            .and_then(<[Value]>::first)
            .and_then(|value| value.get("e"))
            .and_then(Value::as_str);
        assert_eq!(leaf, Some("found"));
    }
}
