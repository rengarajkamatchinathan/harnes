//! A tiny MCP server on stdin/stdout for genie-mcp's integration tests. Not shipped.
//! The behaviour lives in `tests/support/fake_server.rs`, shared with the in-process tests.

#[path = "../../tests/support/fake_server.rs"]
mod fake_server;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let opts = fake_server::Options::from_args(std::env::args().skip(1));
    if fake_server::serve(tokio::io::stdin(), tokio::io::stdout(), opts).await {
        std::process::exit(3);
    }
}
