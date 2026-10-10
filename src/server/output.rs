//! Structured JSON output (lmts `server/output.ts`). Success prints the result as one JSON
//! document; errors print an envelope on stdout (never plain stderr text) and the runner
//! returns a non-zero exit code.

use super::errors::CliError;
use crate::js;
use crate::types::Record;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;

/// `{ "success": false, "error": { "code", "message", "data"? } }`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorEnvelope {
    pub success: bool,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Record>,
}

impl ErrorEnvelope {
    pub fn of(err: &CliError) -> Self {
        ErrorEnvelope {
            success: false,
            error: ErrorBody {
                code: err.code.as_str().into(),
                message: err.message.clone(),
                data: err.data.clone(),
            },
        }
    }
}

/// Write a successful result: `JSON.stringify(result, null, 2)` plus a newline.
pub fn print_result<T: Serialize + ?Sized>(out: &mut dyn Write, result: &T) {
    let _ = writeln!(out, "{}", js::stringify_pretty(result));
}

/// Write an error envelope.
pub fn print_error(out: &mut dyn Write, err: &CliError) {
    let _ = writeln!(out, "{}", js::stringify_pretty(&ErrorEnvelope::of(err)));
}

/// The envelope as a JSON value.
pub fn envelope_value(err: &CliError) -> Value {
    serde_json::to_value(ErrorEnvelope::of(err)).unwrap_or(Value::Null)
}
