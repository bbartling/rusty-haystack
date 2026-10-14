//! Optional, bounded read-only MCP adapters for stdio and local Streamable HTTP.
#[cfg(any(feature = "stdio", feature = "http"))]
mod common;
#[cfg(any(feature = "stdio", feature = "http"))]
mod handler;
#[cfg(any(feature = "stdio", feature = "http"))]
mod schema;
#[cfg(any(feature = "stdio", feature = "http"))]
pub use common::{Limits, McpError, Report};
#[cfg(feature = "stdio")]
mod transport;
#[cfg(feature = "stdio")]
pub use transport::serve;
#[cfg(feature = "http")]
mod auth;
#[cfg(feature = "http")]
mod http_transport;
#[cfg(feature = "http")]
pub use auth::{AuthError, JwtConfig, JwtVerifier, VerificationKey};
#[cfg(feature = "http")]
pub use http_transport::{HttpConfig, serve_http};
/// The exact shared Haystack JSON v4 codec in the bounded MCP page envelope.
pub const OUTPUT_PROFILE: &str = "haystack-h4-json-v1";
/// Exact contextual Jeto, with every scalar boxed and catalog provenance.
pub const JETO_PROFILE: &str = "haystack-jeto-v1";

/// Trusted launch choice. A tool call acknowledges this choice and cannot
/// change its codec, boxing mode or catalog authority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(any(feature = "stdio", feature = "http"), derive(serde::Deserialize))]
pub enum OutputProfile {
    #[default]
    #[cfg_attr(
        any(feature = "stdio", feature = "http"),
        serde(rename = "haystack-h4-json-v1")
    )]
    H4,
    #[cfg_attr(
        any(feature = "stdio", feature = "http"),
        serde(rename = "haystack-jeto-v1")
    )]
    Jeto,
}
impl OutputProfile {
    /// Minimum response budget for the fixed tool inventory, including the
    /// full request-ID allowance and JSON escaping. This does not guarantee
    /// that arbitrary tool results fit the selected response budget.
    pub const fn min_response_bytes(self) -> usize {
        match self {
            Self::H4 => 8 * 1024,
            Self::Jeto => 16 * 1024,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::H4 => OUTPUT_PROFILE,
            Self::Jeto => JETO_PROFILE,
        }
    }
}
