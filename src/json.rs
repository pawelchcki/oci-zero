use core::{char, fmt, str};

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

        let source = self.encoded.as_bytes();
        let mut input = 0;
        let mut output = 0;
        while input < source.len() {
            if source[input] != b'\\' {
                let start = input;
                while input < source.len() && source[input] != b'\\' {
                    input += 1;
                }
                copy_decoded(buffer, &mut output, &source[start..input])?;
                continue;
            }
            let (character, next) = escaped_character(source, input)?;
            input = next;
            let mut encoded = [0; 4];
            copy_decoded(
                buffer,
                &mut output,
                character.encode_utf8(&mut encoded).as_bytes(),
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
        let source = self.encoded.as_bytes();
        let expected = expected.as_bytes();
        let mut input = 0;
        let mut output = 0;
        while input < source.len() {
            if source[input] == b'\\' {
                let Ok((character, next)) = escaped_character(source, input) else {
                    return false;
                };
                if !character.is_ascii() || expected.get(output).copied() != Some(character as u8) {
                    return false;
                }
                input = next;
                output += 1;
            } else {
                if expected.get(output).copied() != Some(source[input]) {
                    return false;
                }
                input += 1;
                output += 1;
            }
        }
        output == expected.len()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonError {
    InvalidUtf8,
    InvalidSyntax,
    NestingTooDeep,
    InvalidEscape,
    InvalidNumber,
    WrongType,
    MissingField(&'static str),
    DuplicateField(&'static str),
    BufferTooSmall,
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => formatter.write_str("JSON is not UTF-8"),
            Self::InvalidSyntax => formatter.write_str("invalid JSON syntax"),
            Self::NestingTooDeep => formatter.write_str("JSON nesting is too deep"),
            Self::InvalidEscape => formatter.write_str("invalid JSON string escape"),
            Self::InvalidNumber => formatter.write_str("invalid JSON number"),
            Self::WrongType => formatter.write_str("unexpected JSON value type"),
            Self::MissingField(field) => write!(formatter, "missing JSON field {field}"),
            Self::DuplicateField(field) => write!(formatter, "duplicate JSON field {field}"),
            Self::BufferTooSmall => formatter.write_str("JSON string output buffer is too small"),
        }
    }
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
        ObjectIter {
            bytes: self.bytes,
            position: 1,
            first: true,
            finished: false,
        }
    }
}

pub(crate) struct ObjectIter<'a> {
    bytes: &'a [u8],
    position: usize,
    first: bool,
    finished: bool,
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = Result<(JsonString<'a>, Value<'a>), JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let result = self.next_inner();
        if result.is_err() {
            self.finished = true;
        }
        result.transpose()
    }
}

impl<'a> ObjectIter<'a> {
    fn next_inner(&mut self) -> Result<Option<(JsonString<'a>, Value<'a>)>, JsonError> {
        self.position = whitespace(self.bytes, self.position);
        if self.bytes.get(self.position) == Some(&b'}') {
            self.finished = true;
            return Ok(None);
        }
        if !self.first {
            if self.bytes.get(self.position) != Some(&b',') {
                return Err(JsonError::InvalidSyntax);
            }
            self.position = whitespace(self.bytes, self.position + 1);
        }
        self.first = false;
        if self.bytes.get(self.position) != Some(&b'"') {
            return Err(JsonError::InvalidSyntax);
        }
        let key_end = parse_string(self.bytes, self.position)?;
        let key = Value {
            bytes: &self.bytes[self.position..key_end],
        }
        .string()?;
        self.position = whitespace(self.bytes, key_end);
        if self.bytes.get(self.position) != Some(&b':') {
            return Err(JsonError::InvalidSyntax);
        }
        let start = whitespace(self.bytes, self.position + 1);
        let end = parse_value(self.bytes, start, 0)?;
        self.position = end;
        Ok(Some((
            key,
            Value {
                bytes: &self.bytes[start..end],
            },
        )))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Array<'a> {
    bytes: &'a [u8],
}

impl<'a> Array<'a> {
    pub(crate) fn iter(self) -> ArrayIter<'a> {
        ArrayIter {
            bytes: self.bytes,
            position: 1,
            first: true,
            finished: false,
        }
    }
}

pub(crate) struct ArrayIter<'a> {
    bytes: &'a [u8],
    position: usize,
    first: bool,
    finished: bool,
}

impl<'a> Iterator for ArrayIter<'a> {
    type Item = Result<Value<'a>, JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let result = self.next_inner();
        if result.is_err() {
            self.finished = true;
        }
        result.transpose()
    }
}

impl<'a> ArrayIter<'a> {
    fn next_inner(&mut self) -> Result<Option<Value<'a>>, JsonError> {
        self.position = whitespace(self.bytes, self.position);
        if self.bytes.get(self.position) == Some(&b']') {
            self.finished = true;
            return Ok(None);
        }
        if !self.first {
            if self.bytes.get(self.position) != Some(&b',') {
                return Err(JsonError::InvalidSyntax);
            }
            self.position = whitespace(self.bytes, self.position + 1);
        }
        self.first = false;
        let start = self.position;
        let end = parse_value(self.bytes, start, 0)?;
        self.position = end;
        Ok(Some(Value {
            bytes: &self.bytes[start..end],
        }))
    }
}

fn parse_value(bytes: &[u8], position: usize, depth: usize) -> Result<usize, JsonError> {
    if depth > MAX_DEPTH {
        return Err(JsonError::NestingTooDeep);
    }
    match bytes.get(position).copied() {
        Some(b'"') => parse_string(bytes, position),
        Some(b'{') => parse_object(bytes, position, depth + 1),
        Some(b'[') => parse_array(bytes, position, depth + 1),
        Some(b't') => literal(bytes, position, b"true"),
        Some(b'f') => literal(bytes, position, b"false"),
        Some(b'n') => literal(bytes, position, b"null"),
        Some(b'-' | b'0'..=b'9') => parse_number(bytes, position),
        _ => Err(JsonError::InvalidSyntax),
    }
}

fn parse_object(bytes: &[u8], mut position: usize, depth: usize) -> Result<usize, JsonError> {
    position = whitespace(bytes, position + 1);
    if bytes.get(position) == Some(&b'}') {
        return Ok(position + 1);
    }
    loop {
        if bytes.get(position) != Some(&b'"') {
            return Err(JsonError::InvalidSyntax);
        }
        position = whitespace(bytes, parse_string(bytes, position)?);
        if bytes.get(position) != Some(&b':') {
            return Err(JsonError::InvalidSyntax);
        }
        position = whitespace(
            bytes,
            parse_value(bytes, whitespace(bytes, position + 1), depth)?,
        );
        match bytes.get(position) {
            Some(b'}') => return Ok(position + 1),
            Some(b',') => position = whitespace(bytes, position + 1),
            _ => return Err(JsonError::InvalidSyntax),
        }
    }
}

fn parse_array(bytes: &[u8], mut position: usize, depth: usize) -> Result<usize, JsonError> {
    position = whitespace(bytes, position + 1);
    if bytes.get(position) == Some(&b']') {
        return Ok(position + 1);
    }
    loop {
        position = whitespace(bytes, parse_value(bytes, position, depth)?);
        match bytes.get(position) {
            Some(b']') => return Ok(position + 1),
            Some(b',') => position = whitespace(bytes, position + 1),
            _ => return Err(JsonError::InvalidSyntax),
        }
    }
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
    match bytes.get(slash + 1).copied() {
        Some(b'"') => Ok(('"', slash + 2)),
        Some(b'\\') => Ok(('\\', slash + 2)),
        Some(b'/') => Ok(('/', slash + 2)),
        Some(b'b') => Ok(('\u{0008}', slash + 2)),
        Some(b'f') => Ok(('\u{000c}', slash + 2)),
        Some(b'n') => Ok(('\n', slash + 2)),
        Some(b'r') => Ok(('\r', slash + 2)),
        Some(b't') => Ok(('\t', slash + 2)),
        Some(b'u') => {
            let first = unicode_escape(bytes, slash + 2)?;
            let next = slash + 6;
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
        _ => Err(JsonError::InvalidEscape),
    }
}

fn unicode_escape(bytes: &[u8], start: usize) -> Result<u16, JsonError> {
    let digits = bytes
        .get(start..start + 4)
        .ok_or(JsonError::InvalidEscape)?;
    let mut value = 0u16;
    for digit in digits {
        value = value
            .checked_mul(16)
            .and_then(|value| hex(*digit).map(|digit| value + u16::from(digit)))
            .ok_or(JsonError::InvalidEscape)?;
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
    match bytes.get(position) {
        Some(b'0') => position += 1,
        Some(b'1'..=b'9') => {
            position += 1;
            while matches!(bytes.get(position), Some(b'0'..=b'9')) {
                position += 1;
            }
        }
        _ => return Err(JsonError::InvalidNumber),
    }
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while matches!(bytes.get(position), Some(b'0'..=b'9')) {
            position += 1;
        }
        if position == start {
            return Err(JsonError::InvalidNumber);
        }
    }
    if matches!(bytes.get(position), Some(b'e' | b'E')) {
        position += 1;
        if matches!(bytes.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        let start = position;
        while matches!(bytes.get(position), Some(b'0'..=b'9')) {
            position += 1;
        }
        if position == start {
            return Err(JsonError::InvalidNumber);
        }
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
    let end = output
        .checked_add(bytes.len())
        .ok_or(JsonError::BufferTooSmall)?;
    let destination = buffer
        .get_mut(*output..end)
        .ok_or(JsonError::BufferTooSmall)?;
    destination.copy_from_slice(bytes);
    *output = end;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{JsonError, Value};

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
}
