use std::{
    fmt,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::FileExt,
};

pub const READ_BUFFER_BYTES: usize = 64 * 1024;

pub struct RecordWindow {
    file: File,
    start: u64,
    content_end: u64,
    next_offset: u64,
    record_hash: String,
    expected_hash: blake3::Hash,
}

impl fmt::Debug for RecordWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordWindow")
            .field("start", &self.start)
            .field("content_end", &self.content_end)
            .field("next_offset", &self.next_offset)
            .field("content_length", &self.content_length())
            .finish_non_exhaustive()
    }
}

impl RecordWindow {
    #[must_use]
    pub const fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub const fn content_end(&self) -> u64 {
        self.content_end
    }

    #[must_use]
    pub const fn content_length(&self) -> u64 {
        self.content_end.saturating_sub(self.start)
    }

    #[must_use]
    pub const fn next_offset(&self) -> u64 {
        self.next_offset
    }

    #[must_use]
    pub fn record_hash(&self) -> &str {
        &self.record_hash
    }

    /// Opens a positional reader over the record's newline-exclusive bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying file handle cannot be cloned.
    pub fn open(&self) -> io::Result<WindowReader> {
        Ok(WindowReader {
            file: self.file.try_clone()?,
            offset: self.start,
            remaining: self.content_length(),
            hasher: blake3::Hasher::new(),
            expected_hash: self.expected_hash,
            verified: false,
        })
    }
}

pub struct WindowReader {
    file: File,
    offset: u64,
    remaining: u64,
    hasher: blake3::Hasher,
    expected_hash: blake3::Hash,
    verified: bool,
}

impl fmt::Debug for WindowReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WindowReader")
            .field("offset", &self.offset)
            .field("remaining", &self.remaining)
            .field("verified", &self.verified)
            .finish_non_exhaustive()
    }
}

impl Read for WindowReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            self.verify_complete()?;
            return Ok(0);
        }
        if output.is_empty() {
            return Ok(0);
        }

        let allowed = match usize::try_from(self.remaining) {
            Ok(remaining) => output.len().min(remaining),
            Err(_) => output.len(),
        };
        let read = self.file.read_at(&mut output[..allowed], self.offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "record window ended before its committed byte range",
            ));
        }
        let read_u64 = u64::try_from(read).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "record window read length does not fit in u64",
            )
        })?;
        let next_offset = self.offset.checked_add(read_u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "record window offset overflow")
        })?;
        let remaining = self.remaining.checked_sub(read_u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "record window read exceeded its byte range",
            )
        })?;
        self.hasher.update(&output[..read]);
        self.offset = next_offset;
        self.remaining = remaining;
        if self.remaining == 0 {
            self.verify_complete()?;
        }
        Ok(read)
    }
}

impl WindowReader {
    fn verify_complete(&mut self) -> io::Result<()> {
        if self.verified {
            return Ok(());
        }
        if self.hasher.finalize() != self.expected_hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "record window hash does not match the committed record",
            ));
        }
        self.verified = true;
        Ok(())
    }
}

#[derive(Debug)]
pub enum ScanOutcome {
    Complete(RecordWindow),
    Incomplete { retry_from: u64 },
    Eof,
}

pub struct RecordScanner {
    file: File,
    cursor: u64,
    target_size: u64,
    buffer: Box<[u8; READ_BUFFER_BYTES]>,
    bytes_read: u64,
}

impl fmt::Debug for RecordScanner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordScanner")
            .field("cursor", &self.cursor)
            .field("target_size", &self.target_size)
            .field("buffer_capacity", &self.buffer.len())
            .field("bytes_read", &self.bytes_read)
            .finish_non_exhaustive()
    }
}

impl RecordScanner {
    /// Creates a scanner at `cursor`, bounded by the committed `target_size`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be positioned at `cursor` or if the
    /// fixed read buffer cannot be initialized at its required size.
    pub fn new(mut file: File, cursor: u64, target_size: u64) -> io::Result<Self> {
        file.seek(SeekFrom::Start(cursor))?;
        let buffer = vec![0_u8; READ_BUFFER_BYTES]
            .into_boxed_slice()
            .try_into()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixed record scanner buffer has the wrong size",
                )
            })?;
        Ok(Self {
            file,
            cursor,
            target_size,
            buffer,
            bytes_read: 0,
        })
    }

    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    #[must_use]
    pub fn buffer_capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Scans the next newline-delimited record or reports a stable retry point.
    ///
    /// # Errors
    ///
    /// Returns an error when reading, seeking, cloning the file handle, or
    /// computing a checked record offset fails.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> io::Result<ScanOutcome> {
        if self.cursor >= self.target_size {
            return Ok(ScanOutcome::Eof);
        }

        let start = self.cursor;
        let window_file = self.file.try_clone()?;
        match self.scan_next(start, window_file) {
            Ok(outcome) => Ok(outcome),
            Err(scan_error) => {
                self.cursor = start;
                match self.file.seek(SeekFrom::Start(start)) {
                    Ok(_) => Err(scan_error),
                    Err(restore_error) => Err(io::Error::new(
                        restore_error.kind(),
                        "record scan failed and scanner position could not be restored",
                    )),
                }
            }
        }
    }

    fn scan_next(&mut self, start: u64, window_file: File) -> io::Result<ScanOutcome> {
        let mut scan_cursor = start;
        let mut hash = blake3::Hasher::new();
        loop {
            let remaining = self.target_size.saturating_sub(scan_cursor);
            if remaining == 0 {
                return self.restore_incomplete(start);
            }

            let capacity = match usize::try_from(remaining) {
                Ok(remaining) => self.buffer.len().min(remaining),
                Err(_) => self.buffer.len(),
            };
            let read = self.file.read(&mut self.buffer[..capacity])?;
            if read == 0 {
                return self.restore_incomplete(start);
            }

            let read_u64 = u64::try_from(read).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record scanner read length does not fit in u64",
                )
            })?;
            self.bytes_read = self.bytes_read.checked_add(read_u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "scanner byte count overflow")
            })?;

            if let Some(index) = self.buffer[..read].iter().position(|byte| *byte == b'\n') {
                hash.update(&self.buffer[..index]);
                let index_u64 = u64::try_from(index).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "record delimiter index does not fit in u64",
                    )
                })?;
                let content_end = scan_cursor.checked_add(index_u64).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "record end offset overflow")
                })?;
                let next_offset = content_end.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "next record offset overflow")
                })?;

                self.file.seek(SeekFrom::Start(next_offset))?;
                self.cursor = next_offset;
                let expected_hash = hash.finalize();
                return Ok(ScanOutcome::Complete(RecordWindow {
                    file: window_file,
                    start,
                    content_end,
                    next_offset,
                    record_hash: format!("b3:{}", expected_hash.to_hex()),
                    expected_hash,
                }));
            }

            hash.update(&self.buffer[..read]);
            scan_cursor = scan_cursor.checked_add(read_u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "record cursor overflow")
            })?;
        }
    }

    fn restore_incomplete(&mut self, retry_from: u64) -> io::Result<ScanOutcome> {
        self.file.seek(SeekFrom::Start(retry_from))?;
        self.cursor = retry_from;
        Ok(ScanOutcome::Incomplete { retry_from })
    }
}
