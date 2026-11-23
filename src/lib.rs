//! cpp-turbo-builder — zero-dependency compile cache + Unity Build orchestrator.
//!
//! Two things live here:
//! - [`proxy`] — "fastcache": hashes a translation unit (source + all its
//!   headers + compiler identity + flags), reuses the ready object file from a
//!   content-addressed cache, and only invokes the real compiler on a miss.
//! - [`orchestrator`] — finds every `.cpp` in a project, glues them into Unity
//!   Build chunks, compiles the chunks in parallel through the proxy and links
//!   the final executable.
//!
//! Everything is implemented on `std` only, so the tool builds offline and has
//! no supply-chain surface at all.

pub mod cachekey;
pub mod compiler;
pub mod config;
pub mod includes;
pub mod orchestrator;
pub mod proxy;
pub mod sha256;
pub mod util;

/// Errors that are about the tool itself (bad usage, IO, cache problems).
/// A *compiler* failure is not an error: it is reported as
/// [`proxy::Outcome::Failed`] and propagated as an exit code.
#[derive(Debug)]
pub enum TurboError {
    Io(std::io::Error),
    Msg(String),
}

pub type TurboResult<T> = std::result::Result<T, TurboError>;

impl std::fmt::Display for TurboError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurboError::Io(e) => write!(f, "{e}"),
            TurboError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for TurboError {}

impl From<std::io::Error> for TurboError {
    fn from(e: std::io::Error) -> Self {
        TurboError::Io(e)
    }
}

impl TurboError {
    pub fn msg(m: impl Into<String>) -> Self {
        TurboError::Msg(m.into())
    }
}
