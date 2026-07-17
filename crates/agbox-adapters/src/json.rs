use std::{
    fmt,
    io::{BufRead, BufReader, Read},
};

use struson::reader::{JsonReader, JsonStreamReader};

use crate::adapter::DecodeError;

pub const MAX_CAPTURE_BYTES: usize = agbox_core::limits::MAX_INLINE_BYTES;
const MAX_FIELD_NAME_BYTES: usize = 128;
const MAX_NESTING_DEPTH: usize = 128;

#[derive(Clone, Eq, PartialEq)]
pub struct CapturedString {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub hash: String,
    pub truncated: bool,
}

impl fmt::Debug for CapturedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedString")
            .field("retained_bytes", &self.bytes.len())
            .field("total_bytes", &self.total_bytes)
            .field("hash", &self.hash)
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Clone, Copy)]
enum ScalarKind {
    Boolean,
    Null,
}

pub struct BoundedJsonReader<R: Read> {
    input: Input<R>,
    parsed: bool,
    schema_fingerprint: Option<String>,
    retained_bytes: usize,
}

impl<R: Read> fmt::Debug for BoundedJsonReader<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedJsonReader")
            .field("parsed", &self.parsed)
            .field("retained_bytes", &self.retained_bytes)
            .field("has_schema_fingerprint", &self.schema_fingerprint.is_some())
            .finish_non_exhaustive()
    }
}

impl<R: Read> BoundedJsonReader<R> {
    #[must_use]
    pub fn new(input: R) -> Self {
        Self {
            input: Input::new(input),
            parsed: false,
            schema_fingerprint: None,
            retained_bytes: 0,
        }
    }

    /// Captures one decoded string while validating and draining the document.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] for malformed JSON, an over-deep document, a
    /// selected non-string value, an I/O failure, or a second parse attempt.
    pub fn capture_string(&mut self, path: &[&str]) -> Result<Option<CapturedString>, DecodeError> {
        let outcome = self.parse(path, SelectionMode::String, MAX_CAPTURE_BYTES)?;
        Ok(outcome.string)
    }

    /// Captures one bounded scalar while validating and draining the document.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when the selected value is a container, exceeds
    /// the scalar bound, or the document is malformed or cannot be read.
    pub fn capture_scalar(
        &mut self,
        path: &[&str],
        limit: usize,
    ) -> Result<Option<String>, DecodeError> {
        if limit > MAX_CAPTURE_BYTES {
            if !self.parsed {
                self.parsed = true;
                if let Err(terminal_error) = self.input.drain_to_terminal() {
                    return Err(DecodeError::Io(terminal_error));
                }
            }
            return Err(DecodeError::OutputTooLarge);
        }
        let outcome = self.parse(path, SelectionMode::Scalar, limit)?;
        outcome
            .scalar
            .map(String::from_utf8)
            .transpose()
            .map_err(|_| DecodeError::Malformed("invalid-decoded-scalar".to_owned()))
    }

    #[must_use]
    pub fn schema_fingerprint(&self) -> Option<&str> {
        self.schema_fingerprint.as_deref()
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn parse(
        &mut self,
        path: &[&str],
        mode: SelectionMode,
        selection_limit: usize,
    ) -> Result<ParseOutcome, DecodeError> {
        if self.parsed {
            return Err(DecodeError::Malformed("already-consumed".to_owned()));
        }
        self.parsed = true;
        let parsed = {
            let mut parser = Parser::new(&mut self.input, path, mode, selection_limit);
            parser.parse_value(0, Some(0)).and_then(|()| {
                parser.input.skip_whitespace()?;
                if parser.input.peek_byte()?.is_some() {
                    return Err(DecodeError::Malformed("trailing-json".to_owned()));
                }
                Ok(parser.finish())
            })
        };
        let outcome = match parsed {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Err(terminal_error) = self.input.drain_to_terminal() {
                    return Err(DecodeError::Io(terminal_error));
                }
                return Err(error);
            }
        };
        self.retained_bytes = outcome.string.as_ref().map_or_else(
            || outcome.scalar.as_ref().map_or(0, Vec::len),
            |value| value.bytes.len(),
        );
        self.schema_fingerprint = Some(outcome.schema_fingerprint.clone());
        Ok(outcome)
    }
}

struct Input<R: Read> {
    reader: BufReader<R>,
    peeked: Option<u8>,
}

impl<R: Read> Input<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(8 * 1024, reader),
            peeked: None,
        }
    }

    fn read_byte(&mut self) -> Result<Option<u8>, DecodeError> {
        if let Some(byte) = self.peeked.take() {
            return Ok(Some(byte));
        }
        let buffer = self.reader.fill_buf()?;
        let Some(byte) = buffer.first().copied() else {
            return Ok(None);
        };
        self.reader.consume(1);
        Ok(Some(byte))
    }

    fn peek_byte(&mut self) -> Result<Option<u8>, DecodeError> {
        if self.peeked.is_none() {
            self.peeked = self.read_byte()?;
        }
        Ok(self.peeked)
    }

    fn required_byte(&mut self) -> Result<u8, DecodeError> {
        self.read_byte()?
            .ok_or_else(|| DecodeError::Malformed("unexpected-eof".to_owned()))
    }

    fn expect(&mut self, expected: u8) -> Result<(), DecodeError> {
        if self.required_byte()? == expected {
            Ok(())
        } else {
            Err(DecodeError::Malformed("unexpected-token".to_owned()))
        }
    }

    fn skip_whitespace(&mut self) -> Result<(), DecodeError> {
        while matches!(self.peek_byte()?, Some(b' ' | b'\n' | b'\r' | b'\t')) {
            let _ = self.read_byte()?;
        }
        Ok(())
    }

    fn drain_to_terminal(&mut self) -> std::io::Result<()> {
        let _ = self.peeked.take();
        loop {
            let buffered = self.reader.fill_buf()?;
            if buffered.is_empty() {
                return Ok(());
            }
            let length = buffered.len();
            self.reader.consume(length);
        }
    }
}

#[derive(Clone, Copy)]
enum SelectionMode {
    String,
    Scalar,
}

struct ParseOutcome {
    string: Option<CapturedString>,
    scalar: Option<Vec<u8>>,
    schema_fingerprint: String,
}

struct Parser<'a, R: Read> {
    input: &'a mut Input<R>,
    path: &'a [&'a str],
    mode: SelectionMode,
    selection_limit: usize,
    selected: bool,
    string: Option<CapturedString>,
    scalar: Option<Vec<u8>>,
    schema: blake3::Hasher,
}

impl<'a, R: Read> Parser<'a, R> {
    fn new(
        input: &'a mut Input<R>,
        path: &'a [&'a str],
        mode: SelectionMode,
        selection_limit: usize,
    ) -> Self {
        let mut schema = blake3::Hasher::new();
        schema.update(b"agbox-schema-v1");
        Self {
            input,
            path,
            mode,
            selection_limit,
            selected: false,
            string: None,
            scalar: None,
            schema,
        }
    }

    fn finish(self) -> ParseOutcome {
        ParseOutcome {
            string: self.string,
            scalar: self.scalar,
            schema_fingerprint: self.schema.finalize().to_hex().to_string(),
        }
    }

    fn parse_value(
        &mut self,
        depth: usize,
        matching_path_index: Option<usize>,
    ) -> Result<(), DecodeError> {
        self.input.skip_whitespace()?;
        let matches_selected_path = matching_path_index == Some(self.path.len());
        if self.selected && matches_selected_path {
            return Err(DecodeError::Malformed(
                "duplicate-selected-field".to_owned(),
            ));
        }
        let selected_here = !self.selected && matches_selected_path;
        match self.input.peek_byte()? {
            Some(b'{') => {
                if selected_here {
                    return Err(DecodeError::Malformed("selected-container".to_owned()));
                }
                self.parse_object(depth, matching_path_index)
            }
            Some(b'[') => {
                if selected_here {
                    return Err(DecodeError::Malformed("selected-container".to_owned()));
                }
                self.parse_array(depth, matching_path_index)
            }
            Some(b'"') => self.parse_string_value(selected_here),
            Some(b't') => self.parse_literal(b"true", ScalarKind::Boolean, selected_here),
            Some(b'f') => self.parse_literal(b"false", ScalarKind::Boolean, selected_here),
            Some(b'n') => self.parse_literal(b"null", ScalarKind::Null, selected_here),
            Some(b'-' | b'0'..=b'9') => self.parse_number(selected_here),
            _ => Err(DecodeError::Malformed("invalid-value".to_owned())),
        }
    }

    fn parse_object(
        &mut self,
        depth: usize,
        matching_path_index: Option<usize>,
    ) -> Result<(), DecodeError> {
        if depth >= MAX_NESTING_DEPTH {
            return Err(DecodeError::Malformed("nesting-depth".to_owned()));
        }
        self.input.expect(b'{')?;
        self.schema.update(b"{");
        self.input.skip_whitespace()?;
        if self.input.peek_byte()? == Some(b'}') {
            let _ = self.input.read_byte()?;
            self.schema.update(b"}");
            return Ok(());
        }
        loop {
            self.input.skip_whitespace()?;
            let name = self.parse_string(MAX_FIELD_NAME_BYTES)?;
            self.schema.update(b"N");
            self.schema.update(&name.total.to_le_bytes());
            self.schema.update(name.hash.as_bytes());
            self.input.skip_whitespace()?;
            self.input.expect(b':')?;
            let child_match = matching_path_index.and_then(|index| {
                self.path
                    .get(index)
                    .filter(|segment| name.equals(segment.as_bytes()))
                    .map(|_| index + 1)
            });
            self.parse_value(depth + 1, child_match)?;
            self.input.skip_whitespace()?;
            match self.input.required_byte()? {
                b',' => {}
                b'}' => {
                    self.schema.update(b"}");
                    return Ok(());
                }
                _ => return Err(DecodeError::Malformed("object-delimiter".to_owned())),
            }
        }
    }

    fn parse_array(
        &mut self,
        depth: usize,
        matching_path_index: Option<usize>,
    ) -> Result<(), DecodeError> {
        if depth >= MAX_NESTING_DEPTH {
            return Err(DecodeError::Malformed("nesting-depth".to_owned()));
        }
        self.input.expect(b'[')?;
        self.schema.update(b"[");
        self.input.skip_whitespace()?;
        if self.input.peek_byte()? == Some(b']') {
            let _ = self.input.read_byte()?;
            self.schema.update(b"]");
            return Ok(());
        }
        loop {
            self.parse_value(depth + 1, matching_path_index)?;
            self.input.skip_whitespace()?;
            match self.input.required_byte()? {
                b',' => {}
                b']' => {
                    self.schema.update(b"]");
                    return Ok(());
                }
                _ => return Err(DecodeError::Malformed("array-delimiter".to_owned())),
            }
        }
    }

    fn parse_string_value(&mut self, selected_here: bool) -> Result<(), DecodeError> {
        self.schema.update(b"S");
        let capture_limit = if selected_here {
            Some(self.selection_limit)
        } else {
            Some(0)
        };
        let value = self.parse_string(capture_limit.unwrap_or_default())?;
        if !selected_here {
            return Ok(());
        }
        self.selected = true;
        match self.mode {
            SelectionMode::String => {
                self.string = Some(CapturedString {
                    bytes: value.prefix,
                    total_bytes: value.total,
                    hash: value.hash,
                    truncated: value.truncated,
                });
            }
            SelectionMode::Scalar => {
                if value.truncated {
                    return Err(DecodeError::OutputTooLarge);
                }
                self.scalar = Some(value.prefix);
            }
        }
        Ok(())
    }

    fn parse_literal(
        &mut self,
        literal: &'static [u8],
        kind: ScalarKind,
        selected_here: bool,
    ) -> Result<(), DecodeError> {
        self.schema.update(match kind {
            ScalarKind::Boolean => b"B",
            ScalarKind::Null => b"Z",
        });
        for expected in literal {
            self.input.expect(*expected)?;
        }
        if selected_here {
            self.selected = true;
            match self.mode {
                SelectionMode::String => {
                    return Err(DecodeError::Malformed("selected-non-string".to_owned()));
                }
                SelectionMode::Scalar => {
                    if literal.len() > self.selection_limit {
                        return Err(DecodeError::OutputTooLarge);
                    }
                    self.scalar = Some(literal.to_vec());
                }
            }
        }
        Ok(())
    }

    fn parse_number(&mut self, selected_here: bool) -> Result<(), DecodeError> {
        self.schema.update(b"D");
        let mut validator = NumberValidator::default();
        let mut selected_bytes = selected_here.then(Vec::new);
        while let Some(byte) = self.input.peek_byte()? {
            if matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                validator.push(byte)?;
                let _ = self.input.read_byte()?;
                if let Some(bytes) = &mut selected_bytes {
                    if bytes.len() == self.selection_limit {
                        return Err(DecodeError::OutputTooLarge);
                    }
                    bytes.push(byte);
                }
            } else {
                break;
            }
        }
        validator.finish()?;
        if let Some(bytes) = selected_bytes {
            self.selected = true;
            match self.mode {
                SelectionMode::String => {
                    return Err(DecodeError::Malformed("selected-non-string".to_owned()));
                }
                SelectionMode::Scalar => {
                    validate_bounded_number_with_struson(&bytes)?;
                    self.scalar = Some(bytes);
                }
            }
        }
        Ok(())
    }

    fn parse_string(&mut self, capture_limit: usize) -> Result<StringInfo, DecodeError> {
        self.input.expect(b'"')?;
        let mut accumulator = StringAccumulator::new(capture_limit);
        loop {
            let byte = self.input.required_byte()?;
            match byte {
                b'"' => return Ok(accumulator.finish()),
                b'\\' => self.parse_escape(&mut accumulator)?,
                0x00..=0x1f => {
                    return Err(DecodeError::Malformed("string-control".to_owned()));
                }
                0x20..=0x7f => accumulator.push(&[byte])?,
                _ => {
                    let length = utf8_sequence_len(byte)
                        .ok_or_else(|| DecodeError::Malformed("invalid-utf8".to_owned()))?;
                    let mut encoded = [0_u8; 4];
                    encoded[0] = byte;
                    for slot in &mut encoded[1..length] {
                        *slot = self.input.required_byte()?;
                    }
                    std::str::from_utf8(&encoded[..length])
                        .map_err(|_| DecodeError::Malformed("invalid-utf8".to_owned()))?;
                    accumulator.push(&encoded[..length])?;
                }
            }
        }
    }

    fn parse_escape(&mut self, accumulator: &mut StringAccumulator) -> Result<(), DecodeError> {
        match self.input.required_byte()? {
            b'"' => accumulator.push(b"\""),
            b'\\' => accumulator.push(b"\\"),
            b'/' => accumulator.push(b"/"),
            b'b' => accumulator.push(&[0x08]),
            b'f' => accumulator.push(&[0x0c]),
            b'n' => accumulator.push(b"\n"),
            b'r' => accumulator.push(b"\r"),
            b't' => accumulator.push(b"\t"),
            b'u' => {
                let first = self.read_hex_quad()?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    self.input.expect(b'\\')?;
                    self.input.expect(b'u')?;
                    let second = self.read_hex_quad()?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(DecodeError::Malformed("invalid-surrogate".to_owned()));
                    }
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return Err(DecodeError::Malformed("invalid-surrogate".to_owned()));
                } else {
                    u32::from(first)
                };
                let character = char::from_u32(scalar)
                    .ok_or_else(|| DecodeError::Malformed("invalid-unicode".to_owned()))?;
                let mut bytes = [0_u8; 4];
                accumulator.push(character.encode_utf8(&mut bytes).as_bytes())
            }
            _ => Err(DecodeError::Malformed("invalid-escape".to_owned())),
        }
    }

    fn read_hex_quad(&mut self) -> Result<u16, DecodeError> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.input.required_byte()?;
            value = value
                .checked_mul(16)
                .and_then(|current| hex_value(digit).map(|part| current + u16::from(part)))
                .ok_or_else(|| DecodeError::Malformed("invalid-hex-escape".to_owned()))?;
        }
        Ok(value)
    }
}

struct StringAccumulator {
    prefix: Vec<u8>,
    capture_limit: usize,
    capture_open: bool,
    total: u64,
    hash: blake3::Hasher,
}

impl StringAccumulator {
    fn new(capture_limit: usize) -> Self {
        Self {
            prefix: Vec::with_capacity(capture_limit.min(MAX_FIELD_NAME_BYTES)),
            capture_limit,
            capture_open: true,
            total: 0,
            hash: blake3::Hasher::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        self.total = self
            .total
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(|_| DecodeError::Malformed("string-length".to_owned()))?,
            )
            .ok_or_else(|| DecodeError::Malformed("string-length".to_owned()))?;
        self.hash.update(bytes);
        if self.capture_open {
            if self.prefix.len().saturating_add(bytes.len()) <= self.capture_limit {
                self.prefix.extend_from_slice(bytes);
            } else {
                self.capture_open = false;
            }
        }
        Ok(())
    }

    fn finish(self) -> StringInfo {
        StringInfo {
            prefix: self.prefix,
            total: self.total,
            hash: self.hash.finalize().to_hex().to_string(),
            truncated: !self.capture_open,
        }
    }
}

struct StringInfo {
    prefix: Vec<u8>,
    total: u64,
    hash: String,
    truncated: bool,
}

impl StringInfo {
    fn equals(&self, expected: &[u8]) -> bool {
        if u64::try_from(expected.len()) != Ok(self.total) {
            return false;
        }
        if expected.len() <= MAX_FIELD_NAME_BYTES {
            return self.prefix == expected;
        }
        self.prefix == expected[..MAX_FIELD_NAME_BYTES]
            && self.hash == blake3::hash(expected).to_hex().as_str()
    }
}

#[derive(Clone, Copy, Default)]
enum NumberState {
    #[default]
    Start,
    Sign,
    Zero,
    Integer,
    Dot,
    Fraction,
    Exponent,
    ExponentSign,
    ExponentDigits,
}

#[derive(Default)]
struct NumberValidator {
    state: NumberState,
}

impl NumberValidator {
    fn push(&mut self, byte: u8) -> Result<(), DecodeError> {
        self.state = match (self.state, byte) {
            (NumberState::Start, b'-') => NumberState::Sign,
            (NumberState::Start | NumberState::Sign, b'0') => NumberState::Zero,
            (NumberState::Start | NumberState::Sign, b'1'..=b'9')
            | (NumberState::Integer, b'0'..=b'9') => NumberState::Integer,
            (NumberState::Zero | NumberState::Integer, b'.') => NumberState::Dot,
            (NumberState::Dot | NumberState::Fraction, b'0'..=b'9') => NumberState::Fraction,
            (NumberState::Zero | NumberState::Integer | NumberState::Fraction, b'e' | b'E') => {
                NumberState::Exponent
            }
            (NumberState::Exponent, b'+' | b'-') => NumberState::ExponentSign,
            (
                NumberState::Exponent | NumberState::ExponentSign | NumberState::ExponentDigits,
                b'0'..=b'9',
            ) => NumberState::ExponentDigits,
            _ => return Err(DecodeError::Malformed("invalid-number".to_owned())),
        };
        Ok(())
    }

    fn finish(self) -> Result<(), DecodeError> {
        if matches!(
            self.state,
            NumberState::Zero
                | NumberState::Integer
                | NumberState::Fraction
                | NumberState::ExponentDigits
        ) {
            Ok(())
        } else {
            Err(DecodeError::Malformed("incomplete-number".to_owned()))
        }
    }
}

fn validate_bounded_number_with_struson(bytes: &[u8]) -> Result<(), DecodeError> {
    let mut reader = JsonStreamReader::new(bytes);
    reader
        .next_number_as_str()
        .map_err(|_| DecodeError::Malformed("invalid-number".to_owned()))?;
    reader
        .consume_trailing_whitespace()
        .map_err(|_| DecodeError::Malformed("invalid-number".to_owned()))
}

fn utf8_sequence_len(first: u8) -> Option<usize> {
    match first {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
