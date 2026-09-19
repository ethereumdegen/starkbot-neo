#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid setting `{field}`: {reason}")]
    InvalidSetting { field: &'static str, reason: String },
    #[error("provider `{provider}` failed: {message}")]
    Provider { provider: String, message: String },
    #[error("operation was cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("provider authentication failed")]
    Authentication,
    #[error("provider request was rate limited")]
    RateLimited,
    #[error("model `{0}` is unavailable")]
    ModelUnavailable(String),
    #[error("provider capability is unavailable: {0}")]
    Unsupported(String),
    #[error("provider transport failed: {0}")]
    Transport(String),
    #[error("provider returned invalid data: {0}")]
    InvalidResponse(String),
}
