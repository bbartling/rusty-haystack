//! Optional MCP adapter. Select `stdio` for the bounded read-only pipe profile.
#[cfg(feature = "stdio")]
mod handler;
#[cfg(feature = "stdio")]
mod transport;
#[cfg(feature = "stdio")]
pub use transport::{Limits, McpError, Report, serve};

/// The exact shared Haystack JSON v4 codec in the bounded MCP page envelope.
pub const OUTPUT_PROFILE: &str = "haystack-h4-json-v1";
