/// Why a native call did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeError {
    /// Not available on this platform, in this build, or in this process (for example
    /// notifications from an unbundled macOS binary). Callers treat it as "skip the feature".
    #[error("not supported on this platform")]
    Unsupported,
    /// The OS or the user refused (keychain locked, permission not granted).
    #[error("denied: {0}")]
    Denied(String),
    /// The user dismissed a dialog.
    #[error("cancelled")]
    Cancelled,
    /// The caller passed something unusable (an invalid accelerator, a URL with a forbidden scheme).
    #[error("invalid: {0}")]
    Invalid(String),
    /// The OS reported a failure; the text is one line and safe to show.
    #[error("{0}")]
    Os(String),
}

pub type Result<T, E = NativeError> = std::result::Result<T, E>;

impl NativeError {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported)
    }
}
