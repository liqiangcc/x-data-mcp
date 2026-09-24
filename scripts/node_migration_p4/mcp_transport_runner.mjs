// Node side of the NM-058 transport diff: feeds stdin lines through
// runStdio over the probe stub handler (same deterministic responses as
// the Rust mcp_server legacy API on an empty store).
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { runStdio } from "../../src_node/mcp/transport.mjs";
import { stubHandler } from "./mcp_transport_stub.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const tools = JSON.parse(readFileSync(join(here, "fixtures/public_tools.json"), "utf8"));
await runStdio(stubHandler(tools));
