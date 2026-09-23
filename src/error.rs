//! Error type for the frontend.
//!
//! The frontend never panics or exits on malformed/unsupported input: every such case returns a
//! [`FrontendError`]. This is essential for soundness — a construct we cannot lower *faithfully*
//! must be refused, not lowered best-effort (see the crate docs).

use std::fmt;

/// Why a SQL pair could not be lowered to the prover's IR.
#[derive(Debug, Clone)]
pub enum FrontendError {
    /// The SQL text could not be parsed (sqlparser error).
    Parse(String),
    /// A construct we deliberately refuse (to preserve soundness) or do not yet support.
    Unsupported(String),
    /// Malformed or incomplete input: unknown table/column, bad DDL, wrong number of queries.
    Schema(String),
    /// The two queries' `$N` placeholders do not line up, so the frontend's index binding (`$N` in one
    /// query is `$N` in the other) cannot be the correspondence the caller means. The message opens
    /// with the sub-reason — `arity` or `order` — see the `params` module and the README's
    /// "Soundness" section, which records what the check cannot catch.
    ///
    /// Also reported *in place of* a refusal that the misalignment itself produced — a
    /// [`FrontendError::Schema`] type conflict (`params::root_cause`), or a
    /// [`FrontendError::Unsupported`] whose message names an inferred type, such as the `LIMIT` count
    /// guard (`params::root_cause_lowered`). Identifying two unrelated values by index is enough to
    /// create either.
    ///
    /// Its own variant rather than an [`FrontendError::Unsupported`] because it is a statement about
    /// the *input*, not about a construct: nothing here is unsupported, and the fix is to renumber
    /// the pair rather than to wait for the frontend to grow a feature.
    ParameterMisaligned(String),
}

impl fmt::Display for FrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrontendError::Parse(m) => write!(f, "PARSE ERROR: {m}"),
            FrontendError::Unsupported(m) => write!(f, "unsupported: {m}"),
            // Schema messages are already self-describing (e.g. "unresolved column ...").
            FrontendError::Schema(m) => write!(f, "{m}"),
            FrontendError::ParameterMisaligned(m) => write!(f, "parameter-misaligned: {m}"),
        }
    }
}

impl std::error::Error for FrontendError {}

pub type Result<T> = std::result::Result<T, FrontendError>;

/// Construct an [`FrontendError::Unsupported`].
pub fn unsupported(msg: impl Into<String>) -> FrontendError {
    FrontendError::Unsupported(msg.into())
}

/// Construct an [`FrontendError::Schema`].
pub fn schema(msg: impl Into<String>) -> FrontendError {
    FrontendError::Schema(msg.into())
}

/// Construct an [`FrontendError::ParameterMisaligned`]. `msg` starts with the sub-reason.
pub fn misaligned(msg: impl Into<String>) -> FrontendError {
    FrontendError::ParameterMisaligned(msg.into())
}
