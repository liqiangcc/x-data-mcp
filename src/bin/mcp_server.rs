use x_data_mcp::{
    mcp::{McpApi, run_stdio},
    task::TaskStore,
    tdx::TdxSource,
};

fn main() -> std::io::Result<()> {
    let root = std::env::var("X_DATA_MCP_STORE").unwrap_or_else(|_| ".x-data-mcp".into());
    let python = std::env::var("X_DATA_MCP_TDX_PYTHON").unwrap_or_else(|_| "python3".into());
    let api = McpApi::new(
        TdxSource::new(python, "scripts/tdx_bridge.py"),
        TaskStore::new(root),
    );
    run_stdio(api)
}
