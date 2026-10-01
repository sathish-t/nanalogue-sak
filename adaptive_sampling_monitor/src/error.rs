//! Small application error helpers built on the standard library.

use std::error::Error as StdError;
use std::fmt::{self, Display};

/// Error type shared by the binary and its tests.
pub(crate) type Error = Box<dyn StdError + Send + Sync>;

/// Result type shared by the binary and its tests.
pub(crate) type Result<T> = std::result::Result<T, Error>;

/// A diagnostic with no underlying error.
#[derive(Debug)]
struct Message(String);

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl StdError for Message {}

/// Creates a plain application error.
pub(crate) fn message(text: impl Into<String>) -> Error {
    Box::new(Message(text.into()))
}

/// Returns an error containing `text` when `condition` is false.
pub(crate) fn ensure(condition: bool, text: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message(text))
    }
}

/// Adds actionable context to optional values and fallible operations.
pub(crate) trait Context<T> {
    /// Uses static context when no value is available or an operation fails.
    fn context(self, text: &str) -> Result<T>;

    /// Lazily constructs context when an operation fails.
    fn with_context<F>(self, make_text: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T> Context<T> for Option<T> {
    fn context(self, text: &str) -> Result<T> {
        self.ok_or_else(|| message(text))
    }

    fn with_context<F>(self, make_text: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.ok_or_else(|| message(make_text()))
    }
}

impl<T, E> Context<T> for std::result::Result<T, E>
where
    E: StdError + Send + Sync + 'static,
{
    fn context(self, text: &str) -> Result<T> {
        self.map_err(|error| message(format!("{text}: {error}")))
    }

    fn with_context<F>(self, make_text: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|error| message(format!("{}: {error}", make_text())))
    }
}
