//! Error codes, exit codes and the structured error type of a CLI server (lmts `server/errors.ts`).

use crate::types::Record;
use serde_json::Value;
use std::fmt;

/// Stable error codes surfaced in JSON error envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    InvalidRequest,
    ValidationError,
    NotFound,
    Conflict,
    Forbidden,
    Timeout,
    ReadOnlyViolation,
    InternalError,
    ServiceUnavailable,
    CommandFailed,
    CommandNotFound,
    MissingConfirmation,
    UnsupportedProductVersion,
    ParseError,
    UnknownError,
}

impl ErrorCode {
    /// Every code, in declaration order.
    pub const ALL: [ErrorCode; 15] = [
        ErrorCode::InvalidRequest,
        ErrorCode::ValidationError,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::Forbidden,
        ErrorCode::Timeout,
        ErrorCode::ReadOnlyViolation,
        ErrorCode::InternalError,
        ErrorCode::ServiceUnavailable,
        ErrorCode::CommandFailed,
        ErrorCode::CommandNotFound,
        ErrorCode::MissingConfirmation,
        ErrorCode::UnsupportedProductVersion,
        ErrorCode::ParseError,
        ErrorCode::UnknownError,
    ];

    /// The envelope string, e.g. `VALIDATION_ERROR`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidRequest => "INVALID_REQUEST",
            ErrorCode::ValidationError => "VALIDATION_ERROR",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::ReadOnlyViolation => "READONLY_VIOLATION",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::ServiceUnavailable => "SERVICE_UNAVAILABLE",
            ErrorCode::CommandFailed => "COMMAND_FAILED",
            ErrorCode::CommandNotFound => "COMMAND_NOT_FOUND",
            ErrorCode::MissingConfirmation => "MISSING_CONFIRMATION",
            ErrorCode::UnsupportedProductVersion => "UNSUPPORTED_PRODUCT_VERSION",
            ErrorCode::ParseError => "PARSE_ERROR",
            ErrorCode::UnknownError => "UNKNOWN_ERROR",
        }
    }

    /// Process exit code (lmapi protocol section 6).
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::InvalidRequest | ErrorCode::ValidationError => 2,
            ErrorCode::NotFound => 4,
            ErrorCode::Conflict => 5,
            ErrorCode::Forbidden | ErrorCode::ReadOnlyViolation => 6,
            ErrorCode::Timeout => 7,
            ErrorCode::ServiceUnavailable => 8,
            ErrorCode::MissingConfirmation => 9,
            ErrorCode::UnsupportedProductVersion => 10,
            ErrorCode::ParseError => 3,
            ErrorCode::CommandNotFound => 127,
            ErrorCode::InternalError | ErrorCode::CommandFailed | ErrorCode::UnknownError => 1,
        }
    }

    /// Parse an envelope string back into a code.
    pub fn parse(s: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured error returned by operations; the runner prints it as a JSON envelope on
/// stdout and exits with its code's exit status.
#[derive(Debug, Clone, PartialEq)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub data: Option<Record>,
}

impl CliError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        CliError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach the envelope's `data` object.
    pub fn with_data(mut self, data: Record) -> Self {
        self.data = Some(data);
        self
    }

    /// Insert one `data` field.
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.data.get_or_insert_with(Record::new).insert(key.to_string(), value);
        self
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// Any other error becomes `UNKNOWN_ERROR` with its message (lmts `toCliError`).
    pub fn from_error(error: &dyn std::error::Error) -> Self {
        let message = error.to_string();
        CliError::new(
            ErrorCode::UnknownError,
            if message.is_empty() {
                "Unknown error".to_string()
            } else {
                message
            },
        )
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::from_error(&e)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliError::from_error(&e)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::new(
            ErrorCode::UnknownError,
            if message.is_empty() {
                "Unknown error".to_string()
            } else {
                message
            },
        )
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        CliError::from(message.to_string())
    }
}
