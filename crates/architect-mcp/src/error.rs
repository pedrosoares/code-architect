//! What can go wrong connecting to an MCP server.

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("could not start the process: {0}")]
    Transport(#[from] rmcp::RmcpError),

    #[error("MCP handshake failed: {0}")]
    Initialize(#[from] rmcp::service::ClientInitializeError),

    #[error("could not list the server's tools: {0}")]
    ListTools(#[from] rmcp::service::ServiceError),
}
