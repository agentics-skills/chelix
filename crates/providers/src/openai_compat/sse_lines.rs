//! Line splitting for SSE byte streams.
//!
//! HTTP chunks arrive at arbitrary byte boundaries, including inside a
//! multi-byte UTF-8 sequence. Bytes are therefore accumulated as bytes and
//! decoded only once a whole line is available, so a chunk boundary can never
//! corrupt the text.

use std::str::Utf8Error;

/// Byte buffer that yields complete, trimmed SSE lines.
#[derive(Debug, Default)]
pub struct SseLineBuffer {
    buf: Vec<u8>,
}

impl SseLineBuffer {
    /// Append a transport chunk.
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Take the next complete line, if one is buffered.
    ///
    /// The line is decoded strictly; invalid UTF-8 is an error rather than a
    /// replacement character.
    pub fn next_line(&mut self) -> Result<Option<String>, Utf8Error> {
        let Some(pos) = self.buf.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let rest = self.buf.split_off(pos + 1);
        let line = std::mem::replace(&mut self.buf, rest);
        Ok(Some(std::str::from_utf8(&line[..pos])?.trim().to_string()))
    }

    /// Take the residual bytes at end of stream as one line.
    pub fn finish(self) -> Result<Option<String>, Utf8Error> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        Ok(Some(std::str::from_utf8(&self.buf)?.trim().to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::SseLineBuffer;

    #[test]
    fn reassembles_a_multibyte_char_split_across_chunks() {
        let bytes = "data: й\n".as_bytes();
        let mut buffer = SseLineBuffer::default();
        buffer.push(&bytes[..7]);
        assert_eq!(buffer.next_line().unwrap(), None);
        buffer.push(&bytes[7..]);

        assert_eq!(buffer.next_line().unwrap().as_deref(), Some("data: й"));
        assert_eq!(buffer.next_line().unwrap(), None);
    }

    #[test]
    fn finish_yields_the_residual_line_without_newline() {
        let bytes = "data: й".as_bytes();
        let mut buffer = SseLineBuffer::default();
        buffer.push(&bytes[..7]);
        buffer.push(&bytes[7..]);

        assert_eq!(buffer.finish().unwrap().as_deref(), Some("data: й"));
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        let mut buffer = SseLineBuffer::default();
        buffer.push(b"data: \xff\n");
        assert!(buffer.next_line().is_err());

        let mut residual = SseLineBuffer::default();
        residual.push(b"data: \xff");
        assert!(residual.finish().is_err());
    }
}
