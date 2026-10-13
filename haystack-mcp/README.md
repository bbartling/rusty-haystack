# rusty-haystack-mcp

Optional read-only MCP adapter over `haystack-app`. There are no default
features. Select `stdio` to expose `serve`, `Limits`, `McpError` and `Report`.
The adapter borrows the managed application/runtime and offers only
`haystack_query` and `haystack_read` using the shared policy, cursors and budgets.

See [the MCP pipe profile](../docs/mcp-stdio.md) for launch configuration, schemas,
lifecycle guarantees, protocol versions and qualification commands. Standalone
consumers install `rusty-haystack-cli` with its `mcp` feature and launch
`haystack mcp stdio --config FILE` with Unix pipe stdin/stdout.
