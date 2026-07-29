//! Core error type for choragos-core.

/// The canonical error type used throughout `choragos-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A required environment variable was absent.
    #[error("missing environment variable: {0}")]
    MissingEnv(String),

    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialisation or deserialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// An external command failed.
    #[error("command error ({context}): {message}")]
    Command {
        /// Short label identifying which command or operation failed.
        context: String,
        /// Human-readable description of the failure.
        message: String,
    },

    /// A generic message-only error.
    #[error("{0}")]
    Message(String),

    /// A requested resource does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A resource already exists (e.g. an idempotent create was attempted
    /// twice).
    #[error("already exists: {0}")]
    AlreadyExists(String),

    /// A failure that is expected to be transient and may succeed if
    /// retried (e.g. a flaky network call).
    #[error("transient error ({context}): {message}")]
    Transient {
        /// Short label identifying which command or operation failed.
        context: String,
        /// Human-readable description of the failure.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::CoreError;

    #[test]
    fn missing_env_display_is_non_empty() {
        let err = CoreError::MissingEnv("MY_VAR".to_string());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn command_display_is_non_empty() {
        let err = CoreError::Command {
            context: "git fetch".to_string(),
            message: "exit code 128".to_string(),
        };
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn message_display_is_non_empty() {
        let err = CoreError::Message("something went wrong".to_string());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn not_found_display_is_non_empty() {
        let err = CoreError::NotFound("plan-ref-123".to_string());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn already_exists_display_is_non_empty() {
        let err = CoreError::AlreadyExists("pull request".to_string());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn transient_display_is_non_empty() {
        let err = CoreError::Transient {
            context: "cerebrum recall".to_string(),
            message: "connection reset".to_string(),
        };
        assert!(!err.to_string().is_empty());
    }
}
