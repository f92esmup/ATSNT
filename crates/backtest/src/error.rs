use thiserror::Error;

pub use application::error::BacktestError;

/// The text of a configuration, with where it came from for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigText<'a> {
    /// What the text is, as an operator would name it (usually a file path).
    pub origin: &'a str,
    /// The text itself.
    pub content: &'a str,
}

/// Why a strategy or its configuration could not be loaded.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoadError {
    /// The configuration could not be read from where it is stored.
    #[error("Failed to read {origin}: {message}")]
    Read {
        /// Where the configuration is stored.
        origin: String,
        /// What the storage reported.
        message: String,
    },
    /// The configuration is not the JSON the loader expects.
    #[error("Failed to parse {what} ({origin}): {message}")]
    Parse {
        /// What was being parsed.
        what: &'static str,
        /// Where the text came from.
        origin: String,
        /// What the parser reported.
        message: String,
    },
    /// A value could not be turned into JSON.
    #[error("Failed to serialize {what}: {message}")]
    Serialize {
        /// What was being serialized.
        what: &'static str,
        /// What the serializer reported.
        message: String,
    },
    /// No strategy is registered under the requested name.
    #[error("{0}")]
    UnknownStrategy(String),
    /// Two sources of the same setting disagree.
    #[error("{0}")]
    Conflict(String),
    /// The deployment gate refused the artifact.
    #[error(
        "the deployment artifact is not deployable: {0}. Pass --allow-unproven-strategy to run \
         it anyway (e.g. to test the mechanics on testnet)"
    )]
    NotDeployable(String),
    /// The strategy emits a kind of decision no execution path accepts.
    #[error("strategy '{0}' emits target-position decisions, which no execution path accepts yet")]
    EmitsTargets(String),
    /// The strategy refused its configuration.
    #[error("Failed to initialize strategy: {0}")]
    Initialize(String),
}
