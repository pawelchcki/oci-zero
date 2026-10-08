use core::{char, str};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonString<'a> {
    encoded: &'a str,
    escaped: bool,
}

impl<'a> JsonString<'a> {
    pub const fn encoded(&self) -> &'a str {
        self.encoded
    }

    pub fn as_str(&self) -> Option<&'a str> {
        (!self.escaped).then_some(self.encoded)
    }

    pub fn decode_into<'buffer>(
        &self,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, JsonError> {
        if !self.escaped {
            let destination = buffer
                .get_mut(..self.encoded.len())
                .ok_or(JsonError::BufferTooSmall)?;
            destination.copy_from_slice(self.encoded.as_bytes());
            return str::from_utf8(destination).map_err(|_| JsonError::InvalidUtf8);
        }

        let mut output = 0;
        for character in self.decoded_chars() {
            let mut encoded = [0; 4];
            copy_decoded(
                buffer,
                &mut output,
                character?.encode_utf8(&mut encoded).as_bytes(),
            )?;
        }
        str::from_utf8(&buffer[..output]).map_err(|_| JsonError::InvalidUtf8)
    }

    pub(crate) fn decoded_eq_ascii(&self, expected: &str) -> bool {
        if !expected.is_ascii() {
            return false;
        }
        if !self.escaped {
            return self.encoded == expected;
        }
        self.decoded_chars().eq(expected.chars().map(Ok))
    }

    fn decoded_chars(&self) -> impl Iterator<Item = Result<char, JsonError>> + '_ {
        let mut source = self.encoded;
        core::iter::from_fn(move || {
            let first = source.chars().next()?;
            let result = if first == '\\' {
                escaped_character(source.as_bytes(), 0)
            } else {
                Ok((first, first.len_utf8()))
            };
            source = match result {
                Ok((_, length)) => &source[length..],
                Err(_) => "",
            };
            Some(result.map(|(character, _)| character))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
pub enum JsonError {
    #[display("JSON is not UTF-8")]
    InvalidUtf8,
    #[display("invalid JSON syntax")]
    InvalidSyntax,
    #[display("JSON nesting is too deep")]
    NestingTooDeep,
    #[display("invalid JSON string escape")]
    InvalidEscape,
    #[display("invalid JSON number")]
    InvalidNumber,
    #[display("unexpected JSON value type")]
    WrongType,
    #[display("missing JSON field {_0}")]
    MissingField(&'static str),
    #[display("duplicate JSON field {_0}")]
    DuplicateField(&'static str),
    #[display("JSON string output buffer is too small")]
    BufferTooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Value<'a> {
    bytes: &'a [u8],
}

impl<'a> Value<'a> {
    pub(crate) fn parse_document(bytes: &'a [u8]) -> Result<Self, JsonError> {
        str::from_utf8(bytes).map_err(|_| JsonError::InvalidUtf8)?;
        let start = whitespace(bytes, 0);
        let end = parse_value(bytes, start, 0)?;
        if whitespace(bytes, end) != bytes.len() {
            return Err(JsonError::InvalidSyntax);
        }
        Ok(Self {
            bytes: &bytes[start..end],
        })
    }

    pub(crate) fn object(self) -> Result<Object<'a>, JsonError> {
        if self.bytes.first() != Some(&b'{') {
            return Err(JsonError::WrongType);
        }
        Ok(Object { bytes: self.bytes })
    }

    pub(crate) fn array(self) -> Result<Array<'a>, JsonError> {
        if self.bytes.first() != Some(&b'[') {
            return Err(JsonError::WrongType);
        }
        Ok(Array { bytes: self.bytes })
    }

    pub(crate) fn string(self) -> Result<JsonString<'a>, JsonError> {
        if self.bytes.first() != Some(&b'"') || self.bytes.last() != Some(&b'"') {
            return Err(JsonError::WrongType);
        }
        let encoded = str::from_utf8(&self.bytes[1..self.bytes.len() - 1])
            .map_err(|_| JsonError::InvalidUtf8)?;
        Ok(JsonString {
            encoded,
            escaped: encoded.as_bytes().contains(&b'\\'),
        })
    }

    pub(crate) fn u64(self) -> Result<u64, JsonError> {
        let value = str::from_utf8(self.bytes).map_err(|_| JsonError::InvalidUtf8)?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(JsonError::WrongType);
        }
        value.parse().map_err(|_| JsonError::InvalidNumber)
    }

    pub(crate) fn is_null(self) -> bool {
        self.bytes == b"null"
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Object<'a> {
    bytes: &'a [u8],
}

impl<'a> Object<'a> {
    pub(crate) fn get(self, name: &'static str) -> Result<Option<Value<'a>>, JsonError> {
        let mut found = None;
        for member in self.iter() {
            let (key, value) = member?;
            if key.decoded_eq_ascii(name) {
                if found.is_some() {
                    return Err(JsonError::DuplicateField(name));
                }
                found = Some(value);
            }
        }
        Ok(found)
    }

    pub(crate) fn required(self, name: &'static str) -> Result<Value<'a>, JsonError> {
        self.get(name)?.ok_or(JsonError::MissingField(name))
    }

    pub(crate) fn iter(self) -> ObjectIter<'a> {
        ObjectIter(Container::new(self.bytes, 0, b'}'))
    }
}

pub(crate) struct ObjectIter<'a>(Container<'a>);

impl<'a> Iterator for ObjectIter<'a> {
    type Item = Result<(JsonString<'a>, Value<'a>), JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next(0).map(|member| {
            let (key, value) = member?;
            Ok((key.expect("object member has a key"), value))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Array<'a> {
    bytes: &'a [u8],
}

impl<'a> Array<'a> {
    pub(crate) fn iter(self) -> ArrayIter<'a> {
        ArrayIter(Container::new(self.bytes, 0, b']'))
    }
}

pub(crate) struct ArrayIter<'a>(Container<'a>);

impl<'a> Iterator for ArrayIter<'a> {
    type Item = Result<Value<'a>, JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next(0).map(|member| member.map(|(_, value)| value))
    }
}

/// Shared cursor for validating containers and iterating their borrowed values.
struct Container<'a> {
    bytes: &'a [u8],
    closing: u8,
    first: bool,
    finished: bool,
}

type Member<'a> = (Option<JsonString<'a>>, Value<'a>);

impl<'a> Container<'a> {
    fn new(bytes: &'a [u8], opening: usize, closing: u8) -> Self {
        Self {
            bytes: &bytes[opening + 1..],
            closing,
            first: true,
            finished: false,
        }
    }

    fn next(&mut self, depth: usize) -> Option<Result<Member<'a>, JsonError>> {
        if self.finished {
            return None;
        }
        let result = self.next_inner(depth);
        if result.is_err() {
            self.finished = true;
        }
        result.transpose()
    }

    fn next_inner(&mut self, depth: usize) -> Result<Option<Member<'a>>, JsonError> {
        self.bytes = &self.bytes[whitespace(self.bytes, 0)..];
        if self.bytes.first() == Some(&self.closing) {
            self.bytes = &self.bytes[1..];
            self.finished = true;
            return Ok(None);
        }
        if !self.first {
            if self.bytes.first() != Some(&b',') {
                return Err(JsonError::InvalidSyntax);
            }
            self.bytes = &self.bytes[whitespace(self.bytes, 1)..];
        }
        self.first = false;
        let key = if self.closing == b'}' {
            if self.bytes.first() != Some(&b'"') {
                return Err(JsonError::InvalidSyntax);
            }
            let end = parse_string(self.bytes, 0)?;
            let key = Value {
                bytes: &self.bytes[..end],
            }
            .string()?;
            self.bytes = &self.bytes[whitespace(self.bytes, end)..];
            if self.bytes.first() != Some(&b':') {
                return Err(JsonError::InvalidSyntax);
            }
            self.bytes = &self.bytes[whitespace(self.bytes, 1)..];
            Some(key)
        } else {
            None
        };
        let end = parse_value(self.bytes, 0, depth)?;
        let value = Value {
            bytes: &self.bytes[..end],
        };
        self.bytes = &self.bytes[end..];
        Ok(Some((key, value)))
    }
}

fn parse_value(bytes: &[u8], position: usize, depth: usize) -> Result<usize, JsonError> {
    if depth > MAX_DEPTH {
        return Err(JsonError::NestingTooDeep);
    }
    match bytes.get(position).copied() {
        Some(b'"') => parse_string(bytes, position),
        Some(b'{') => parse_container(bytes, position, b'}', depth + 1),
        Some(b'[') => parse_container(bytes, position, b']', depth + 1),
        Some(b't') => literal(bytes, position, b"true"),
        Some(b'f') => literal(bytes, position, b"false"),
        Some(b'n') => literal(bytes, position, b"null"),
        Some(b'-' | b'0'..=b'9') => parse_number(bytes, position),
        _ => Err(JsonError::InvalidSyntax),
    }
}

fn parse_container(
    bytes: &[u8],
    position: usize,
    closing: u8,
    depth: usize,
) -> Result<usize, JsonError> {
    let mut container = Container::new(bytes, position, closing);
    while let Some(member) = container.next(depth) {
        member?;
    }
    Ok(bytes.len() - container.bytes.len())
}

fn parse_string(bytes: &[u8], position: usize) -> Result<usize, JsonError> {
    let mut index = position + 1;
    while let Some(byte) = bytes.get(index).copied() {
        match byte {
            b'"' => return Ok(index + 1),
            b'\\' => {
                let (_, next) = escaped_character(bytes, index)?;
                index = next;
            }
            0..=0x1f => return Err(JsonError::InvalidSyntax),
            _ => index += 1,
        }
    }
    Err(JsonError::InvalidSyntax)
}

fn escaped_character(bytes: &[u8], slash: usize) -> Result<(char, usize), JsonError> {
    let character = match bytes.get(slash + 1).copied() {
        Some(value @ (b'"' | b'\\' | b'/')) => char::from(value),
        Some(b'b') => '\u{0008}',
        Some(b'f') => '\u{000c}',
        Some(b'n') => '\n',
        Some(b'r') => '\r',
        Some(b't') => '\t',
        Some(b'u') => return unicode_character(bytes, slash + 2),
        _ => return Err(JsonError::InvalidEscape),
    };
    Ok((character, slash + 2))
}

fn unicode_character(bytes: &[u8], start: usize) -> Result<(char, usize), JsonError> {
    let first = unicode_escape(bytes, start)?;
    let next = start + 4;
    let scalar = if (0xd800..=0xdbff).contains(&first) {
        if bytes.get(next..next + 2) != Some(b"\\u") {
            return Err(JsonError::InvalidEscape);
        }
        let second = unicode_escape(bytes, next + 2)?;
        if !(0xdc00..=0xdfff).contains(&second) {
            return Err(JsonError::InvalidEscape);
        }
        let high = u32::from(first - 0xd800);
        let low = u32::from(second - 0xdc00);
        (0x10000 + (high << 10) + low, next + 6)
    } else if (0xdc00..=0xdfff).contains(&first) {
        return Err(JsonError::InvalidEscape);
    } else {
        (u32::from(first), next)
    };
    let character = char::from_u32(scalar.0).ok_or(JsonError::InvalidEscape)?;
    Ok((character, scalar.1))
}

fn unicode_escape(bytes: &[u8], start: usize) -> Result<u16, JsonError> {
    let digits = bytes
        .get(start..start + 4)
        .ok_or(JsonError::InvalidEscape)?;
    let mut value = 0u16;
    for digit in digits {
        // Exactly four hex digits fit in u16.
        value = (value << 4) | u16::from(hex(*digit).ok_or(JsonError::InvalidEscape)?);
    }
    Ok(value)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_number(bytes: &[u8], mut position: usize) -> Result<usize, JsonError> {
    if bytes.get(position) == Some(&b'-') {
        position += 1;
    }
    position = match bytes.get(position) {
        Some(b'0') => position + 1,
        Some(b'1'..=b'9') => decimal_digits(bytes, position)?,
        _ => return Err(JsonError::InvalidNumber),
    };
    if bytes.get(position) == Some(&b'.') {
        position = decimal_digits(bytes, position + 1)?;
    }
    if matches!(bytes.get(position), Some(b'e' | b'E')) {
        position += 1;
        if matches!(bytes.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        position = decimal_digits(bytes, position)?;
    }
    Ok(position)
}

fn decimal_digits(bytes: &[u8], start: usize) -> Result<usize, JsonError> {
    let mut position = start;
    while matches!(bytes.get(position), Some(b'0'..=b'9')) {
        position += 1;
    }
    if position == start {
        return Err(JsonError::InvalidNumber);
    }
    Ok(position)
}

fn literal(bytes: &[u8], position: usize, literal: &[u8]) -> Result<usize, JsonError> {
    if bytes.get(position..position + literal.len()) == Some(literal) {
        Ok(position + literal.len())
    } else {
        Err(JsonError::InvalidSyntax)
    }
}

fn whitespace(bytes: &[u8], mut position: usize) -> usize {
    while matches!(bytes.get(position), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        position += 1;
    }
    position
}

fn copy_decoded(buffer: &mut [u8], output: &mut usize, bytes: &[u8]) -> Result<(), JsonError> {
    crate::buffer::append(buffer, output, bytes).map_err(|_| JsonError::BufferTooSmall)
}

#[cfg(test)]
mod tests {
    use std::{format, string::ToString};

    use super::{parse_value, JsonError, Value, MAX_DEPTH};

    #[test]
    fn validates_and_iterates_nested_json() {
        let value = Value::parse_document(br#" {"a":[1,{"b":true}],"skip":null} "#).unwrap();
        let object = value.object().unwrap();
        let array = object.required("a").unwrap().array().unwrap();
        assert_eq!(array.iter().count(), 2);
        assert!(object.get("missing").unwrap().is_none());
    }

    #[test]
    fn decodes_strings_and_rejects_bad_surrogates() {
        let value = Value::parse_document(br#""hello\n\u263a\ud83d\ude00""#).unwrap();
        let string = value.string().unwrap();
        let mut output = [0; 32];
        assert_eq!(string.decode_into(&mut output).unwrap(), "hello\n☺😀");
        assert_eq!(
            Value::parse_document(br#""\ud83d!""#),
            Err(JsonError::InvalidEscape)
        );
    }

    #[test]
    fn rejects_trailing_and_duplicate_requested_fields() {
        assert!(Value::parse_document(b"{} x").is_err());
        let object = Value::parse_document(br#"{"a":1,"\u0061":2}"#)
            .unwrap()
            .object()
            .unwrap();
        assert_eq!(object.get("a"), Err(JsonError::DuplicateField("a")));
    }

    #[test]
    fn decodes_every_simple_escape_and_uppercase_hex() {
        let value = Value::parse_document(br#""\"\\\/\b\f\n\r\t\u004A\u00AF""#).unwrap();
        let string = value.string().unwrap();
        let mut output = [0; 32];
        assert_eq!(
            string.decode_into(&mut output).unwrap().as_bytes(),
            b"\"\\/\x08\x0c\n\r\tJ\xc2\xaf"
        );

        let positioned_backslash = Value::parse_document(br#""a\\b""#)
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(
            positioned_backslash.decode_into(&mut output).unwrap(),
            "a\\b"
        );
    }

    #[test]
    fn exposes_unescaped_strings_and_checks_decoded_ascii_exactly() {
        let plain = Value::parse_document(br#""plain""#)
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(plain.encoded(), "plain");
        assert_eq!(plain.as_str(), Some("plain"));
        assert!(plain.decoded_eq_ascii("plain"));
        assert!(!plain.decoded_eq_ascii("other"));
        assert!(!plain.decoded_eq_ascii("£"));

        let escaped = Value::parse_document(br#""\u0062""#)
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(escaped.as_str(), None);
        assert!(escaped.decoded_eq_ascii("b"));
        assert!(!escaped.decoded_eq_ascii("a"));
    }

    #[test]
    fn rejects_raw_control_characters_and_small_decode_buffers() {
        assert_eq!(
            Value::parse_document(b"\"line\nfeed\""),
            Err(JsonError::InvalidSyntax)
        );

        let plain = Value::parse_document(br#""hello""#)
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(
            plain.decode_into(&mut [0; 4]),
            Err(JsonError::BufferTooSmall)
        );
        let escaped = Value::parse_document(br#""\u263a""#)
            .unwrap()
            .string()
            .unwrap();
        assert_eq!(
            escaped.decode_into(&mut [0; 2]),
            Err(JsonError::BufferTooSmall)
        );
    }

    #[test]
    fn accepts_empty_containers_and_false_literal() {
        assert_eq!(
            Value::parse_document(b"{}")
                .unwrap()
                .object()
                .unwrap()
                .iter()
                .count(),
            0
        );
        assert_eq!(
            Value::parse_document(b"[]")
                .unwrap()
                .array()
                .unwrap()
                .iter()
                .count(),
            0
        );
        assert!(Value::parse_document(b"false").is_ok());
    }

    #[test]
    fn rejects_malformed_container_members_and_fuses_iterators() {
        for bytes in [
            b"[1,]".as_slice(),
            b"[,1]",
            b"[1 2]",
            b"[1",
            b"{a:1}",
            b"{\"a\" 1}",
            b"{\"a\":}",
            b"{\"a\":1,}",
            b"{\"a\":1 \"b\":2}",
        ] {
            assert_eq!(
                Value::parse_document(bytes),
                Err(JsonError::InvalidSyntax),
                "{bytes:?}"
            );
        }
        let mut array = Value { bytes: b"[1,]" }.array().unwrap().iter();
        assert_eq!(array.next().unwrap().unwrap().u64(), Ok(1));
        assert_eq!(array.next(), Some(Err(JsonError::InvalidSyntax)));
        assert_eq!(array.next(), None);
        let mut object = Value {
            bytes: b"{\"a\" 1}",
        }
        .object()
        .unwrap()
        .iter();
        assert_eq!(object.next(), Some(Err(JsonError::InvalidSyntax)));
        assert_eq!(object.next(), None);
    }

    #[test]
    fn enforces_nesting_at_the_exact_limit() {
        assert_eq!(parse_value(b"null", 0, MAX_DEPTH), Ok(4));
        assert_eq!(
            parse_value(b"null", 0, MAX_DEPTH + 1),
            Err(JsonError::NestingTooDeep)
        );
        assert_eq!(
            parse_value(b"null", 0, MAX_DEPTH + 2),
            Err(JsonError::NestingTooDeep)
        );

        let arrays_at_limit = format!("{}null{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(Value::parse_document(arrays_at_limit.as_bytes()).is_ok());
        let arrays_too_deep = format!(
            "{}null{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert_eq!(
            Value::parse_document(arrays_too_deep.as_bytes()),
            Err(JsonError::NestingTooDeep)
        );

        let objects_at_limit = format!(
            "{}null{}",
            "{\"a\":".repeat(MAX_DEPTH),
            "}".repeat(MAX_DEPTH)
        );
        assert!(Value::parse_document(objects_at_limit.as_bytes()).is_ok());
        let objects_too_deep = format!(
            "{}null{}",
            "{\"a\":".repeat(MAX_DEPTH + 1),
            "}".repeat(MAX_DEPTH + 1)
        );
        assert_eq!(
            Value::parse_document(objects_too_deep.as_bytes()),
            Err(JsonError::NestingTooDeep)
        );
    }

    #[test]
    fn validates_each_json_number_form() {
        for number in ["0", "-1", "10", "1.25", "1e2", "1E+2", "1e-2"] {
            assert!(
                Value::parse_document(number.as_bytes()).is_ok(),
                "number={number:?}"
            );
        }
        for number in ["-", "01", "1.", "1e", "1e+", "1e-"] {
            assert!(
                Value::parse_document(number.as_bytes()).is_err(),
                "number={number:?}"
            );
        }

        assert_eq!(Value { bytes: b"" }.u64(), Err(JsonError::WrongType));
        assert_eq!(Value { bytes: b"12x" }.u64(), Err(JsonError::WrongType));
        assert_eq!(Value { bytes: b"42" }.u64(), Ok(42));
        assert_eq!(
            Value {
                bytes: b"18446744073709551616"
            }
            .u64(),
            Err(JsonError::InvalidNumber)
        );
    }

    #[test]
    fn string_conversion_checks_both_quotes() {
        assert_eq!(
            Value { bytes: b"plain\"" }.string(),
            Err(JsonError::WrongType)
        );
        assert_eq!(
            Value { bytes: b"\"plain" }.string(),
            Err(JsonError::WrongType)
        );
    }

    #[test]
    fn formats_every_json_error() {
        let cases = [
            (JsonError::InvalidUtf8, "JSON is not UTF-8"),
            (JsonError::InvalidSyntax, "invalid JSON syntax"),
            (JsonError::NestingTooDeep, "JSON nesting is too deep"),
            (JsonError::InvalidEscape, "invalid JSON string escape"),
            (JsonError::InvalidNumber, "invalid JSON number"),
            (JsonError::WrongType, "unexpected JSON value type"),
            (JsonError::MissingField("name"), "missing JSON field name"),
            (
                JsonError::DuplicateField("name"),
                "duplicate JSON field name",
            ),
            (
                JsonError::BufferTooSmall,
                "JSON string output buffer is too small",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }
}
