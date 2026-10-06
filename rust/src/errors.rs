//! Typed errors so every failure carries a stable `errorKind` for orchestrators.

use std::error::Error as StdError;

use serde_json::{Map, Value};

/// Stable failure categories reported in the result line.
pub const ERROR_KINDS: [&str; 8] = [
    "not-found",
    "permission",
    "conflict",
    "config",
    "target",
    "startup",
    "unconfined",
    "unknown",
];

/// One of [`ERROR_KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotFound,
    Permission,
    Conflict,
    Config,
    Target,
    Startup,
    Unconfined,
    Unknown,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::NotFound => "not-found",
            ErrorKind::Permission => "permission",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Config => "config",
            ErrorKind::Target => "target",
            ErrorKind::Startup => "startup",
            ErrorKind::Unconfined => "unconfined",
            ErrorKind::Unknown => "unknown",
        }
    }

    /// Parses a kind name; anything unrecognised is `unknown`.
    pub fn parse(name: &str) -> ErrorKind {
        match name {
            "not-found" => ErrorKind::NotFound,
            "permission" => ErrorKind::Permission,
            "conflict" => ErrorKind::Conflict,
            "config" => ErrorKind::Config,
            "target" => ErrorKind::Target,
            "startup" => ErrorKind::Startup,
            "unconfined" => ErrorKind::Unconfined,
            _ => ErrorKind::Unknown,
        }
    }
}

/// The fields of a [`PluginError`].
#[derive(Debug)]
pub struct ErrorDetails {
    pub kind: ErrorKind,
    /// Human-readable explanation.
    pub message: String,
    /// Captured CLI output, added to the failure result line.
    pub output: String,
    /// Fields added to the failure result line.
    pub payload: Option<Map<String, Value>>,
    pub cause: Option<Box<dyn StdError + Send + Sync>>,
}

/// Error with a stable machine-readable kind and optional captured CLI output.
/// Boxed, so a `Result<T, PluginError>` stays a single pointer wide on the error side.
#[derive(Debug)]
pub struct PluginError(Box<ErrorDetails>);

impl std::ops::Deref for PluginError {
    type Target = ErrorDetails;

    fn deref(&self) -> &ErrorDetails {
        &self.0
    }
}

impl std::ops::DerefMut for PluginError {
    fn deref_mut(&mut self) -> &mut ErrorDetails {
        &mut self.0
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0.message)
    }
}

impl StdError for PluginError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0
            .cause
            .as_deref()
            .map(|cause| cause as &(dyn StdError + 'static))
    }
}

pub type Result<T> = std::result::Result<T, PluginError>;

impl PluginError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        PluginError(Box::new(ErrorDetails {
            kind,
            message: message.into(),
            output: String::new(),
            payload: None,
            cause: None,
        }))
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.0.output = output.into();
        self
    }

    pub fn with_payload(mut self, payload: Map<String, Value>) -> Self {
        self.0.payload = Some(payload);
        self
    }

    pub fn with_cause(mut self, cause: impl StdError + Send + Sync + 'static) -> Self {
        self.0.cause = Some(Box::new(cause));
        self
    }

    pub fn kind_str(&self) -> &'static str {
        self.0.kind.as_str()
    }

    /// Whether the underlying cause is an I/O error of the given kind.
    pub fn cause_is_io(&self, kind: std::io::ErrorKind) -> bool {
        self.0
            .cause
            .as_deref()
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .is_some_and(|error| error.kind() == kind)
    }
}

/// The error kind of any error, defaulting to `unknown`.
pub fn error_kind_of(error: &(dyn StdError + 'static)) -> ErrorKind {
    error
        .downcast_ref::<PluginError>()
        .map_or(ErrorKind::Unknown, |error| error.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_unknown_names_fall_back() {
        for name in ERROR_KINDS {
            assert_eq!(ErrorKind::parse(name).as_str(), name);
        }
        assert_eq!(ErrorKind::parse("bogus"), ErrorKind::Unknown);
    }

    #[test]
    fn io_cause_is_detected() {
        let error = PluginError::new(ErrorKind::Startup, "x")
            .with_cause(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(error.cause_is_io(std::io::ErrorKind::NotFound));
        assert!(!error.cause_is_io(std::io::ErrorKind::PermissionDenied));
        assert_eq!(error_kind_of(&error), ErrorKind::Startup);
    }
}
