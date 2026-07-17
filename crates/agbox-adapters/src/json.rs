use std::{
    fmt,
    io::{BufRead, BufReader, Read},
};

use struson::reader::{JsonReader, JsonStreamReader};
use zeroize::Zeroizing;

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

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct SecureCapturedString {
    pub bytes: Zeroizing<Vec<u8>>,
    pub total_bytes: u64,
    pub hash: String,
    pub truncated: bool,
}

impl fmt::Debug for SecureCapturedString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecureCapturedString")
            .field("retained_bytes", &self.bytes.len())
            .field("total_bytes", &self.total_bytes)
            .field("hash", &self.hash)
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl SecureCapturedString {
    pub(crate) fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.bytes)
    }

    pub(crate) fn take_hash(&mut self) -> String {
        std::mem::take(&mut self.hash)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum CapturedValue {
    String(SecureCapturedString),
    Scalar(String),
    Container,
}

impl fmt::Debug for CapturedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => formatter.debug_tuple("String").field(value).finish(),
            Self::Scalar(value) => formatter
                .debug_struct("Scalar")
                .field("byte_length", &value.len())
                .finish(),
            Self::Container => formatter.write_str("Container"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapturedMatch {
    pub array_indices: Vec<usize>,
    pub value: CapturedValue,
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

    /// Captures repeated scalar values at one object path, retaining their
    /// enclosing array ordinals so callers can correlate sibling projections.
    pub(crate) fn capture_matches(
        &mut self,
        path: &[&str],
        limit: usize,
        max_matches: usize,
        max_retained_bytes: usize,
    ) -> Result<Vec<CapturedMatch>, DecodeError> {
        if limit > MAX_CAPTURE_BYTES || max_matches > agbox_core::limits::MAX_EVENTS_PER_RECORD {
            if !self.parsed {
                self.parsed = true;
                if let Err(terminal_error) = self.input.drain_to_terminal() {
                    return Err(DecodeError::Io(terminal_error));
                }
            }
            return Err(DecodeError::OutputTooLarge);
        }
        let outcome = self.parse(
            path,
            SelectionMode::Matches {
                max_matches,
                max_retained_bytes,
            },
            limit,
        )?;
        Ok(outcome.matches)
    }

    /// Hashes complete decoded string matches in document order, inserting one
    /// newline between matches, while retaining only a bounded UTF-8 prefix.
    pub(crate) fn capture_joined_matches(
        &mut self,
        path: &[&str],
        selected_indices: &[Vec<usize>],
        limit: usize,
        max_matches: usize,
    ) -> Result<Option<SecureCapturedString>, DecodeError> {
        if limit > MAX_CAPTURE_BYTES
            || max_matches > agbox_core::limits::MAX_EVENTS_PER_RECORD
            || selected_indices.len() > max_matches
        {
            if !self.parsed {
                self.parsed = true;
                if let Err(terminal_error) = self.input.drain_to_terminal() {
                    return Err(DecodeError::Io(terminal_error));
                }
            }
            return Err(DecodeError::OutputTooLarge);
        }
        let outcome = self.parse_selected(
            path,
            SelectionMode::JoinedMatches { max_matches },
            limit,
            selected_indices,
        )?;
        Ok(outcome.joined)
    }

    /// Captures complete selected JSON values as bounded raw JSON prefixes.
    /// Hash and byte length cover the whole selected value, including
    /// structured objects or arrays.
    pub(crate) fn capture_raw_matches(
        &mut self,
        path: &[&str],
        limit: usize,
        max_matches: usize,
        max_retained_bytes: usize,
    ) -> Result<Vec<CapturedMatch>, DecodeError> {
        if limit > MAX_CAPTURE_BYTES || max_matches > agbox_core::limits::MAX_EVENTS_PER_RECORD {
            if !self.parsed {
                self.parsed = true;
                if let Err(terminal_error) = self.input.drain_to_terminal() {
                    return Err(DecodeError::Io(terminal_error));
                }
            }
            return Err(DecodeError::OutputTooLarge);
        }
        let outcome = self.parse(
            path,
            SelectionMode::RawMatches {
                max_matches,
                max_retained_bytes,
            },
            limit,
        )?;
        Ok(outcome.matches)
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
        self.parse_selected(path, mode, selection_limit, &[])
    }

    fn parse_selected(
        &mut self,
        path: &[&str],
        mode: SelectionMode,
        selection_limit: usize,
        selected_indices: &[Vec<usize>],
    ) -> Result<ParseOutcome, DecodeError> {
        if self.parsed {
            return Err(DecodeError::Malformed("already-consumed".to_owned()));
        }
        self.parsed = true;
        let parsed = {
            let mut parser = Parser::new(
                &mut self.input,
                path,
                mode,
                selection_limit,
                selected_indices,
            );
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
        self.retained_bytes = outcome
            .matches
            .iter()
            .try_fold(self.retained_bytes, |total, captured| {
                let bytes = match &captured.value {
                    CapturedValue::String(value) => value.bytes.len(),
                    CapturedValue::Scalar(value) => value.len(),
                    CapturedValue::Container => 0,
                };
                total.checked_add(bytes)
            })
            .ok_or(DecodeError::OutputTooLarge)?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(outcome.joined.as_ref().map_or(0, |value| value.bytes.len()))
            .ok_or(DecodeError::OutputTooLarge)?;
        self.schema_fingerprint = Some(outcome.schema_fingerprint.clone());
        Ok(outcome)
    }
}

struct Input<R: Read> {
    reader: BufReader<R>,
    peeked: Option<u8>,
    capture: Option<StringAccumulator>,
}

impl<R: Read> Input<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::with_capacity(8 * 1024, reader),
            peeked: None,
            capture: None,
        }
    }

    fn read_byte(&mut self) -> Result<Option<u8>, DecodeError> {
        if let Some(byte) = self.peeked.take() {
            self.capture_byte(byte)?;
            return Ok(Some(byte));
        }
        let buffer = self.reader.fill_buf()?;
        let Some(byte) = buffer.first().copied() else {
            return Ok(None);
        };
        self.reader.consume(1);
        self.capture_byte(byte)?;
        Ok(Some(byte))
    }

    fn peek_byte(&mut self) -> Result<Option<u8>, DecodeError> {
        if self.peeked.is_none() {
            let buffer = self.reader.fill_buf()?;
            let Some(byte) = buffer.first().copied() else {
                return Ok(None);
            };
            self.reader.consume(1);
            self.peeked = Some(byte);
        }
        Ok(self.peeked)
    }

    fn capture_byte(&mut self, byte: u8) -> Result<(), DecodeError> {
        if let Some(capture) = &mut self.capture {
            capture.push(&[byte])?;
        }
        Ok(())
    }

    fn start_capture(&mut self, limit: usize) -> Result<(), DecodeError> {
        if self.capture.is_some() {
            return Err(DecodeError::Malformed("nested-selection".to_owned()));
        }
        self.capture = Some(StringAccumulator::new(limit));
        Ok(())
    }

    fn finish_capture(&mut self) -> Result<StringInfo, DecodeError> {
        self.capture
            .take()
            .map(StringAccumulator::finish)
            .ok_or_else(|| DecodeError::Malformed("missing-selection".to_owned()))
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
    Matches {
        max_matches: usize,
        max_retained_bytes: usize,
    },
    RawMatches {
        max_matches: usize,
        max_retained_bytes: usize,
    },
    JoinedMatches {
        max_matches: usize,
    },
}

struct ParseOutcome {
    string: Option<CapturedString>,
    scalar: Option<Vec<u8>>,
    matches: Vec<CapturedMatch>,
    joined: Option<SecureCapturedString>,
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
    matches: Vec<CapturedMatch>,
    array_indices: Vec<usize>,
    selected_indices: &'a [Vec<usize>],
    joined: Option<StringAccumulator>,
    joined_matches: usize,
    schema: blake3::Hasher,
}

impl<'a, R: Read> Parser<'a, R> {
    fn new(
        input: &'a mut Input<R>,
        path: &'a [&'a str],
        mode: SelectionMode,
        selection_limit: usize,
        selected_indices: &'a [Vec<usize>],
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
            matches: Vec::new(),
            array_indices: Vec::new(),
            selected_indices,
            joined: matches!(mode, SelectionMode::JoinedMatches { .. })
                .then(|| StringAccumulator::new(selection_limit)),
            joined_matches: 0,
            schema,
        }
    }

    fn finish(mut self) -> ParseOutcome {
        let joined = if self.joined_matches == 0 {
            None
        } else {
            self.joined.take().map(|accumulator| {
                let mut value = accumulator.finish();
                SecureCapturedString {
                    bytes: value.take_secure_prefix(),
                    total_bytes: value.total,
                    hash: value.take_hash(),
                    truncated: value.truncated,
                }
            })
        };
        ParseOutcome {
            string: self.string,
            scalar: self.scalar,
            matches: self.matches,
            joined,
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
        let matches_selected_index = self.selected_indices.is_empty()
            || self
                .selected_indices
                .iter()
                .any(|indices| indices == &self.array_indices);
        if self.selected
            && matches_selected_path
            && matches_selected_index
            && !matches!(
                self.mode,
                SelectionMode::Matches { .. }
                    | SelectionMode::RawMatches { .. }
                    | SelectionMode::JoinedMatches { .. }
            )
        {
            return Err(DecodeError::Malformed(
                "duplicate-selected-field".to_owned(),
            ));
        }
        let selected_here = matches_selected_path
            && matches_selected_index
            && (!self.selected
                || matches!(
                    self.mode,
                    SelectionMode::Matches { .. }
                        | SelectionMode::RawMatches { .. }
                        | SelectionMode::JoinedMatches { .. }
                ));
        if selected_here
            && let SelectionMode::RawMatches {
                max_matches,
                max_retained_bytes,
            } = self.mode
        {
            let remaining = max_retained_bytes.saturating_sub(self.retained_match_bytes()?);
            self.input
                .start_capture(self.selection_limit.min(remaining))?;
            self.parse_unselected_value(depth, None)?;
            let mut value = self.input.finish_capture()?;
            let mut prefix = value.take_secure_prefix();
            if let Err(error) = std::str::from_utf8(&prefix) {
                prefix.truncate(error.valid_up_to());
            }
            let retained = self
                .matches
                .iter()
                .try_fold(prefix.len(), |total, captured| {
                    let bytes = match &captured.value {
                        CapturedValue::String(value) => value.bytes.len(),
                        CapturedValue::Scalar(value) => value.len(),
                        CapturedValue::Container => 0,
                    };
                    total.checked_add(bytes)
                })
                .ok_or(DecodeError::OutputTooLarge)?;
            if retained > max_retained_bytes {
                return Err(DecodeError::OutputTooLarge);
            }
            self.push_match(
                max_matches,
                CapturedValue::String(SecureCapturedString {
                    bytes: prefix,
                    total_bytes: value.total,
                    hash: value.take_hash(),
                    truncated: value.truncated,
                }),
            )?;
            return Ok(());
        }
        if selected_here
            && let SelectionMode::Matches { max_matches, .. } = self.mode
            && matches!(self.input.peek_byte()?, Some(b'{' | b'['))
        {
            self.push_match(max_matches, CapturedValue::Container)?;
            return self.parse_unselected_value(depth, None);
        }
        self.parse_unselected_or_selected_value(depth, matching_path_index, selected_here)
    }

    fn parse_unselected_value(
        &mut self,
        depth: usize,
        matching_path_index: Option<usize>,
    ) -> Result<(), DecodeError> {
        self.parse_unselected_or_selected_value(depth, matching_path_index, false)
    }

    fn parse_unselected_or_selected_value(
        &mut self,
        depth: usize,
        matching_path_index: Option<usize>,
        selected_here: bool,
    ) -> Result<(), DecodeError> {
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
        let mut index = 0_usize;
        loop {
            self.array_indices.push(index);
            self.parse_value(depth + 1, matching_path_index)?;
            let _ = self.array_indices.pop();
            self.input.skip_whitespace()?;
            match self.input.required_byte()? {
                b',' => {
                    index = index.checked_add(1).ok_or(DecodeError::OutputTooLarge)?;
                }
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
        if selected_here && let SelectionMode::JoinedMatches { max_matches } = self.mode {
            if self.joined_matches == max_matches {
                return Err(DecodeError::OutputTooLarge);
            }
            let mut joined = self
                .joined
                .take()
                .ok_or_else(|| DecodeError::Malformed("joined-selection-state".to_owned()))?;
            if self.joined_matches > 0 {
                joined.push(b"\n")?;
            }
            self.parse_string_into(&mut joined)?;
            self.joined = Some(joined);
            self.joined_matches = self
                .joined_matches
                .checked_add(1)
                .ok_or(DecodeError::OutputTooLarge)?;
            self.selected = true;
            return Ok(());
        }
        let capture_limit = if selected_here {
            Some(match self.mode {
                SelectionMode::Matches {
                    max_retained_bytes, ..
                } => self
                    .selection_limit
                    .min(max_retained_bytes.saturating_sub(self.retained_match_bytes()?)),
                _ => self.selection_limit,
            })
        } else {
            Some(0)
        };
        let mut value = self.parse_string(capture_limit.unwrap_or_default())?;
        if !selected_here {
            return Ok(());
        }
        self.selected = true;
        match self.mode {
            SelectionMode::String => {
                self.string = Some(CapturedString {
                    bytes: value.take_prefix(),
                    total_bytes: value.total,
                    hash: value.take_hash(),
                    truncated: value.truncated,
                });
            }
            SelectionMode::Scalar => {
                if value.truncated {
                    return Err(DecodeError::OutputTooLarge);
                }
                self.scalar = Some(value.take_prefix());
            }
            SelectionMode::Matches { max_matches, .. } => {
                self.push_match(
                    max_matches,
                    CapturedValue::String(SecureCapturedString {
                        bytes: value.take_secure_prefix(),
                        total_bytes: value.total,
                        hash: value.take_hash(),
                        truncated: value.truncated,
                    }),
                )?;
            }
            SelectionMode::RawMatches { .. } => {
                return Err(DecodeError::Malformed("raw-selection-state".to_owned()));
            }
            SelectionMode::JoinedMatches { .. } => {
                return Err(DecodeError::Malformed("joined-selection-state".to_owned()));
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
                SelectionMode::String | SelectionMode::JoinedMatches { .. } => {
                    return Err(DecodeError::Malformed("selected-non-string".to_owned()));
                }
                SelectionMode::Scalar => {
                    if literal.len() > self.selection_limit {
                        return Err(DecodeError::OutputTooLarge);
                    }
                    self.scalar = Some(literal.to_vec());
                }
                SelectionMode::Matches { max_matches, .. } => {
                    self.push_match(
                        max_matches,
                        CapturedValue::Scalar(
                            std::str::from_utf8(literal)
                                .map_err(|_| {
                                    DecodeError::Malformed("invalid-decoded-scalar".to_owned())
                                })?
                                .to_owned(),
                        ),
                    )?;
                }
                SelectionMode::RawMatches { .. } => {
                    return Err(DecodeError::Malformed("raw-selection-state".to_owned()));
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
                SelectionMode::String | SelectionMode::JoinedMatches { .. } => {
                    return Err(DecodeError::Malformed("selected-non-string".to_owned()));
                }
                SelectionMode::Scalar => {
                    validate_bounded_number_with_struson(&bytes)?;
                    self.scalar = Some(bytes);
                }
                SelectionMode::Matches { max_matches, .. } => {
                    let value = String::from_utf8(bytes)
                        .map_err(|_| DecodeError::Malformed("invalid-decoded-scalar".to_owned()))?;
                    self.push_match(max_matches, CapturedValue::Scalar(value))?;
                }
                SelectionMode::RawMatches { .. } => {
                    return Err(DecodeError::Malformed("raw-selection-state".to_owned()));
                }
            }
        }
        Ok(())
    }

    fn push_match(&mut self, max_matches: usize, value: CapturedValue) -> Result<(), DecodeError> {
        if self.matches.len() == max_matches {
            return Err(DecodeError::OutputTooLarge);
        }
        let retained_limit = match self.mode {
            SelectionMode::Matches {
                max_retained_bytes, ..
            }
            | SelectionMode::RawMatches {
                max_retained_bytes, ..
            } => max_retained_bytes,
            SelectionMode::String | SelectionMode::Scalar | SelectionMode::JoinedMatches { .. } => {
                usize::MAX
            }
        };
        let value_bytes = match &value {
            CapturedValue::String(value) => value.bytes.len(),
            CapturedValue::Scalar(value) => value.len(),
            CapturedValue::Container => 0,
        };
        if self
            .retained_match_bytes()?
            .checked_add(value_bytes)
            .is_none_or(|bytes| bytes > retained_limit)
        {
            return Err(DecodeError::OutputTooLarge);
        }
        self.matches.push(CapturedMatch {
            array_indices: self.array_indices.clone(),
            value,
        });
        Ok(())
    }

    fn retained_match_bytes(&self) -> Result<usize, DecodeError> {
        self.matches.iter().try_fold(0_usize, |total, captured| {
            let bytes = match &captured.value {
                CapturedValue::String(value) => value.bytes.len(),
                CapturedValue::Scalar(value) => value.len(),
                CapturedValue::Container => 0,
            };
            total.checked_add(bytes).ok_or(DecodeError::OutputTooLarge)
        })
    }

    fn parse_string(&mut self, capture_limit: usize) -> Result<StringInfo, DecodeError> {
        let mut accumulator = StringAccumulator::new(capture_limit);
        self.parse_string_into(&mut accumulator)?;
        Ok(accumulator.finish())
    }

    fn parse_string_into(
        &mut self,
        accumulator: &mut StringAccumulator,
    ) -> Result<(), DecodeError> {
        self.input.expect(b'"')?;
        loop {
            let byte = self.input.required_byte()?;
            match byte {
                b'"' => return Ok(()),
                b'\\' => self.parse_escape(accumulator)?,
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
    prefix: Zeroizing<Vec<u8>>,
    capture_limit: usize,
    capture_open: bool,
    total: u64,
    hash: blake3::Hasher,
}

impl StringAccumulator {
    fn new(capture_limit: usize) -> Self {
        Self {
            prefix: Zeroizing::new(Vec::with_capacity(capture_limit.min(MAX_FIELD_NAME_BYTES))),
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

    fn finish(mut self) -> StringInfo {
        StringInfo {
            prefix: std::mem::take(&mut self.prefix),
            total: self.total,
            hash: self.hash.finalize().to_hex().to_string(),
            truncated: !self.capture_open,
        }
    }
}

struct StringInfo {
    prefix: Zeroizing<Vec<u8>>,
    total: u64,
    hash: String,
    truncated: bool,
}

impl StringInfo {
    fn take_prefix(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.prefix)
    }

    fn take_secure_prefix(&mut self) -> Zeroizing<Vec<u8>> {
        std::mem::take(&mut self.prefix)
    }

    fn take_hash(&mut self) -> String {
        std::mem::take(&mut self.hash)
    }

    fn equals(&self, expected: &[u8]) -> bool {
        if u64::try_from(expected.len()) != Ok(self.total) {
            return false;
        }
        if expected.len() <= MAX_FIELD_NAME_BYTES {
            return self.prefix.as_slice() == expected;
        }
        expected.starts_with(self.prefix.as_slice())
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

#[cfg(test)]
mod tests {
    use super::{StringAccumulator, Zeroizing};

    fn assert_zeroizing_vec(_: &Zeroizing<Vec<u8>>) {}

    #[test]
    fn joined_plaintext_storage_is_zeroizing_from_allocation() {
        let accumulator = StringAccumulator::new(64);
        assert_zeroizing_vec(&accumulator.prefix);
        let info = accumulator.finish();
        assert_zeroizing_vec(&info.prefix);
    }
}
