//! Line-delimited JSON framing: bounded reads, one compact message per line and
//! a flush after every message. Nothing here knows about MCP methods.

use crate::error::{Error, Result};
use serde_json::Value;
use std::io::{self, BufRead, Write};

/// Reads LF-terminated messages (CRLF is accepted) of at most `max_input` bytes
/// and passes each parsed JSON value to `handle`; the optional answer is written
/// as one line. A malformed line is answered with a parse error and the session
/// continues. An oversized line or an unterminated final line is answered with a
/// parse error and ends the session with [`Error::InvalidFraming`].
pub fn serve_lines(
    mut reader: impl BufRead,
    mut writer: impl Write,
    max_input: usize,
    max_output: usize,
    mut handle: impl FnMut(Value) -> Option<Value>,
) -> Result<()> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            if oversized || !line.is_empty() {
                let message = if oversized {
                    "message exceeds configured size limit"
                } else {
                    "unterminated JSON-RPC message"
                };
                emit(&mut writer, parse_error(message), max_output)?;
                return Err(Error::InvalidFraming);
            }
            writer.flush()?;
            return Ok(());
        }
        let newline = bytes.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        let remaining = max_input - line.len();
        line.extend_from_slice(&bytes[..count.min(remaining)]);
        oversized |= count > remaining;
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_none() {
            continue;
        }
        if oversized {
            emit(
                &mut writer,
                parse_error("message exceeds configured size limit"),
                max_output,
            )?;
            return Err(Error::InvalidFraming);
        }
        let response = match serde_json::from_slice(&line) {
            Ok(request) => handle(request),
            Err(_) => Some(parse_error("parse error")),
        };
        if let Some(response) = response {
            emit(&mut writer, response, max_output)?;
        }
        line.clear();
    }
}

fn parse_error(message: &str) -> Value {
    crate::protocol::error_response(Value::Null, -32700, message)
}

fn emit(writer: &mut impl Write, message: Value, max_output: usize) -> io::Result<()> {
    let mut encoded = message.to_string();
    if encoded.len() > max_output {
        encoded = crate::protocol::replace_oversized(&message, max_output).to_string();
    }
    writer.write_all(encoded.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
