use x_data_mcp::{
    mcp::{McpApi, run_stdio},
    official_files::OfficialFilePipeline,
    task::TaskStore,
};

fn main() -> std::io::Result<()> {
    let root = std::env::var("X_DATA_MCP_STORE").unwrap_or_else(|_| ".x-data-mcp".into());
    let api = McpApi::new(OfficialFilePipeline, TaskStore::new(root));
    run_stdio(api)
}
