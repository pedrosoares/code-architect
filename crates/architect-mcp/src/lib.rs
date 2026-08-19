//! An MCP **client** — connects to external MCP servers over stdio and turns
//! their tools into `architect_tools::Tool` implementations, so the agent
//! calls them exactly like `read_file`/`write_file`/etc.
//!
//! Depends on `architect-tools` to implement its `Tool` trait — the same
//! relationship `architect-tools` itself has with `architect-agent`'s
//! `ToolExecutor`, filling an extension point rather than the other way
//! round. Does not depend on `architect-config` or `apps/desktop`:
//! `apps/desktop` is the one place that knows saved server configs, MCP
//! connections, and the tool registry all exist, the same role it already
//! plays bridging `architect-session` and `architect-tools`.
//!
//! Two transports are supported: [`connect`] spawns a local process and
//! talks stdio; [`connect_http`] talks to a remote server over streamable
//! HTTP, with an optional static Bearer token (no OAuth flow). Everything
//! downstream of transport construction — the handshake, tool discovery,
//! adapting tools — is identical between them.

mod adapter;
mod error;

use std::{collections::HashMap, sync::Arc};

use architect_tools::Tool;
use rmcp::{
    RmcpError, RoleClient, ServiceExt,
    service::RunningService,
    transport::{
        ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use tokio::process::Command;

pub use error::McpError;

/// One live connection to an MCP server, plus the tools it exposed at
/// connect time. Dropping this ends the connection — `RunningService` owns
/// the child process and its stdio pipes, so a reconnect is just "build a
/// new one and let the old one drop".
pub struct McpConnection {
    // Never read directly — held only to keep the connection alive for as
    // long as this value lives; every tool call goes through the `Peer`
    // clones each `McpToolAdapter` in `tools` already holds.
    _service: RunningService<RoleClient, ()>,
    pub tools: Vec<Arc<dyn Tool>>,
}

/// Spawn `command` (with `args`/`env`), complete the MCP handshake, and
/// discover every tool the server exposes.
pub async fn connect(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<McpConnection, McpError> {
    let args = args.to_vec();
    let env = env.clone();

    let transport = TokioChildProcess::new(Command::new(command).configure(move |cmd| {
        cmd.args(&args);
        for (key, value) in &env {
            cmd.env(key, value);
        }
    }))
    .map_err(RmcpError::transport_creation::<TokioChildProcess>)?;

    let service = ().serve(transport).await?;
    let discovered = service.list_all_tools().await?;
    let peer = service.peer().clone();

    let tools = discovered
        .into_iter()
        .map(|tool| Arc::new(adapter::McpToolAdapter::new(peer.clone(), tool)) as Arc<dyn Tool>)
        .collect();

    Ok(McpConnection {
        _service: service,
        tools,
    })
}

/// Connect to a remote MCP server over streamable HTTP, complete the
/// handshake, and discover every tool it exposes. `bearer_token`, if given,
/// is sent as a plain Bearer token — no OAuth flow.
pub async fn connect_http(
    url: &str,
    bearer_token: Option<&str>,
) -> Result<McpConnection, McpError> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    if let Some(token) = bearer_token {
        config = config.auth_header(token.to_owned());
    }
    let transport = StreamableHttpClientTransport::from_config(config);

    let service = ().serve(transport).await?;
    let discovered = service.list_all_tools().await?;
    let peer = service.peer().clone();

    let tools = discovered
        .into_iter()
        .map(|tool| Arc::new(adapter::McpToolAdapter::new(peer.clone(), tool)) as Arc<dyn Tool>)
        .collect();

    Ok(McpConnection {
        _service: service,
        tools,
    })
}
