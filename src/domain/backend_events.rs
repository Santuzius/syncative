//! Unified event taxonomy emitted by every backend adapter.
//!
//! Translators (in `crate::infrastructure::translators`) map
//! backend-specific error strings and log lines into these variants so
//! upper layers never parse raw strings. The `Operation` context tells the
//! translator what the caller was trying to do — useful for cases where the
//! same error string has different semantics depending on context (e.g.
//! "already exists" is success for mkdir but an error for upload).

/// What the caller was doing when an error occurred. Passed to
/// `EventTranslator::classify` so translators can give context-aware
/// answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Mkdir,
    Upload,
    Download,
    Delete,
    List,
    Stat,
    Auth,
    Other,
}

/// A structured event produced by a backend adapter. The top-level sync
/// engine and state machine consume these instead of raw error strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendEvent {
    /// The session/token is expired or revoked — user must reauthenticate.
    AuthExpired,
    /// The backend is throttling requests. The pass should be flagged as
    /// degraded; the scheduler will back off on the next cycle.
    RateLimited,
    /// The target already exists. For `Mkdir` this is treated as success
    /// (idempotent); for other operations it is surfaced as an error.
    AlreadyExists,
    /// The target was not found.
    NotFound,
    /// A transient network or transport error — may resolve on retry.
    TransientNetwork,
    /// The error doesn't match a known pattern. The raw message is preserved
    /// so it can be shown to the user or written to the log.
    Other(String),
}

/// Per-backend classifier. Implementations live in
/// `crate::infrastructure::translators`.
pub trait EventTranslator: Send + Sync {
    /// Classify an error string `msg` produced during `op` into a
    /// `BackendEvent`. The default implementation always returns
    /// `BackendEvent::Other(msg.to_owned())`.
    fn classify(&self, op: Operation, msg: &str) -> BackendEvent {
        let _ = op;
        BackendEvent::Other(msg.to_owned())
    }

    /// Return `true` when `msg` (from stderr or an error return) indicates
    /// an auth failure. Shortcut for callers that only need a boolean.
    fn is_auth_failure(&self, msg: &str) -> bool {
        matches!(
            self.classify(Operation::Auth, msg),
            BackendEvent::AuthExpired
        )
    }
}
