//! Round-trips against a real MCP server, ignored by default.
//!
//! `@modelcontextprotocol/server-everything` is the official demo/test
//! server rmcp's own examples use — `npx` installs and runs it on first use,
//! no manual setup beyond having Node/npm. Run with:
//!
//! ```sh
//! cargo test -p architect-mcp -- --ignored --nocapture
//! ```

use std::{collections::HashMap, time::Duration};

#[tokio::test]
async fn connecting_to_a_nonexistent_command_fails_fast() {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        architect_mcp::connect("definitely-not-a-real-binary", &[], &HashMap::new()),
    )
    .await
    .expect("connect must not hang on a command that can't even spawn");

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "spawns a real MCP server via `npx`"]
async fn connects_lists_tools_and_calls_one() {
    let connection = architect_mcp::connect(
        "npx",
        &[
            "-y".to_owned(),
            "@modelcontextprotocol/server-everything".to_owned(),
        ],
        &HashMap::new(),
    )
    .await
    .expect("connect to the everything server");

    assert!(
        !connection.tools.is_empty(),
        "the everything server exposes several tools"
    );

    let add = connection
        .tools
        .iter()
        .find(|tool| tool.name() == "get-sum")
        .unwrap_or_else(|| {
            let names: Vec<_> = connection.tools.iter().map(|tool| tool.name()).collect();
            panic!("no `get-sum` tool; the server actually exposes: {names:?}")
        });
    println!("get-sum input schema: {}", add.input_schema());

    // `ToolContext` is unused by an MCP adapter (see its docs), so any
    // workspace root works here.
    let workspace = tempfile::tempdir().expect("tempdir");
    let ctx = architect_tools::ToolContext::new(workspace.path()).expect("tool context");

    let output = add
        .call(serde_json::json!({"a": 5, "b": 3}), &ctx)
        .await
        .expect("get-sum(5, 3) should succeed");

    assert!(
        output.text.contains('8'),
        "expected the sum of 5 and 3 in the result, got: {}",
        output.text
    );
}
