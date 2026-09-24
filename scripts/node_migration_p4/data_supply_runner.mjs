// Stdio runner for the Node DataApi skeleton — wires DataApi to the
// NM-058 transport (runStdio) the same way mcp_data.rs wires the Rust
// api. Local-owner access; repository seam = RepositorySeam.
//
//   node data_supply_runner.mjs   (newline-delimited JSON-RPC on stdin)

import { runStdio } from "../../src_node/mcp/transport.mjs";
import { DataApi } from "../../src_node/data_supply/mcp.mjs";
import { Access } from "../../src_node/data_supply/access.mjs";

const api = new DataApi({ access: Access.local(), marketSource: null });
await runStdio(api);
