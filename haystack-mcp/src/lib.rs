//! Optional, bounded read-only MCP adapters for stdio and local Streamable HTTP.
#[cfg(any(feature = "stdio", feature = "http"))]
mod common;
#[cfg(any(feature = "stdio", feature = "http"))]
mod handler;
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
