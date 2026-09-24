// Node port of src/acquire/mcp.rs — the x-data-acquire protocol adapter:
// typed tool dispatch over AcquireService. One descriptor table drives
// tools/list, dispatch and capability reporting — no second schema
// source. Outputs are facts only (state, outcome, refs, evidence,
// reason codes); nothing emits next actions or orchestration hints.
//
// ToolHandler contract (w2 transport, src/mcp.rs:629-632):
//   tools() -> the tools array; call(name, args) -> result Value or
//   throws ApiError. ApiError must be THE transport class for
//   `instanceof` routing — inject it at integration:
//   `new AcquireApi(service, { ApiError })` importing
//   ../mcp/transport.mjs. The local DefaultApiError below is
//   shape-identical ({code,message,toJSON}) so the adapter is usable
//   standalone until that module lands on this branch (NM-066).

import { parse as parseWire, serJson } from "./wire.mjs";
import { GENERATION_CONTRACT_VERSION, taskView } from "./service.mjs";
import { createHash } from "node:crypto";

export const CAPABILITY_REVISION = "x-data-acquire-v1";
// env!("CARGO_PKG_VERSION") — pinned identically to w2's transport
// SERVER_VERSION so both implementations answer identically.
const PKG_VERSION = "0.2.0-preview.24";

// Shape-identical to transport.mjs ApiError ({code,message,toJSON}) —
// inject the real class at integration (see header).
export class DefaultApiError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
  toJSON() { return { code: this.code, message: this.message }; }
}

const sha256hex = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");

// ---- schemars schema_for!(T) golden bytes (captured from Rust
// serialization — see evidence/node-migration/p4/acquire_api/). Each
// schema object is stored as parsed JSON; byte-parity rides on the
// transport serializer's BTreeMap key ordering (serde_json Value).
// Populated after the golden dump lands — see acquire_api_probe.mjs.
export const SCHEMAS =   {
    "CheckOfficialUpdates": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        }
      },
      "required": [
        "contract_version"
      ],
      "title": "CheckOfficialUpdates",
      "type": "object"
    },
    "AcquireGeneration": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "request_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "request_id"
      ],
      "title": "AcquireGeneration",
      "type": "object"
    },
    "GetAcquisitionTask": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "task_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "task_id"
      ],
      "title": "GetAcquisitionTask",
      "type": "object"
    },
    "GetAcquisitionRequest": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "request_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "request_id"
      ],
      "title": "GetAcquisitionRequest",
      "type": "object"
    },
    "VerifyGeneration": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "generation_id": {
          "type": "string"
        },
        "request_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "request_id",
        "generation_id"
      ],
      "title": "VerifyGeneration",
      "type": "object"
    },
    "ActivateGeneration": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "expected_previous": {
          "description": "Expected currently-active generation id; `null` means \"none active\"\n(first activation). Any mismatch fails closed (D5).",
          "type": [
            "string",
            "null"
          ]
        },
        "generation_id": {
          "type": "string"
        },
        "request_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "request_id",
        "generation_id"
      ],
      "title": "ActivateGeneration",
      "type": "object"
    },
    "RetireGeneration": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "confirm_token": {
          "description": "Explicit confirmation token: the generation's own manifest digest.\nAnything else refuses the destructive retire (D5).",
          "type": "string"
        },
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        },
        "generation_id": {
          "type": "string"
        },
        "request_id": {
          "type": "string"
        }
      },
      "required": [
        "contract_version",
        "request_id",
        "generation_id",
        "confirm_token"
      ],
      "title": "RetireGeneration",
      "type": "object"
    },
    "SyncStatus": {
      "$defs": {
        "ContractVersion": {
          "enum": [
            "v1"
          ],
          "type": "string"
        }
      },
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "properties": {
        "contract_version": {
          "$ref": "#/$defs/ContractVersion"
        }
      },
      "required": [
        "contract_version"
      ],
      "title": "SyncStatus",
      "type": "object"
    },
    "WorkerOnce": {
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "additionalProperties": false,
      "description": "Operator/worker turn: advances the oldest open acquisition task by one\ndurable stage. Empty input — the runtime worker loop calls this by\nname with no arguments.",
      "title": "WorkerOnce",
      "type": "object"
    }
  };

const ENTRIES = [
  ["check_official_updates",
    "Read-only remote probe of the declared official files. Reports per-role change state (changed/unchanged/unknown) from persisted observations; unknown is a stop signal, never a prompt to act.",
    "CheckOfficialUpdates"],
  ["acquire_generation",
    "Accept a durable download+install task for a fresh official-file generation. request_id binds the acceptance idempotently; downloading never implies verified or active.",
    "AcquireGeneration"],
  ["get_acquisition_task",
    "Read one acquisition task's durable state, stage and terminal error.",
    "GetAcquisitionTask"],
  ["get_acquisition_request",
    "Read the recorded binding and terminal outcome of a request_id without accepting work.",
    "GetAcquisitionRequest"],
  ["verify_generation",
    "Run the installed generation through the structural probe suite and the file-consistency contract; records the once-written decision artifact. NOT_VERIFIED/CONFLICT are stable terminal verdicts.",
    "VerifyGeneration"],
  ["activate_generation",
    "Conditional activation: only a consistency_verified generation may swap the atomic active pointer, and only when expected_previous still matches. Refusals are durable outcomes.",
    "ActivateGeneration"],
  ["retire_generation",
    "Destructive retire to evidence_only: requires the generation's manifest digest as confirm_token, refuses active or referenced generations. Audit artifacts remain; raw bytes are deleted.",
    "RetireGeneration"],
  ["sync_status",
    "Read the active pointer, every recorded generation lifecycle state and inflight acquisition tasks.",
    "SyncStatus"],
  ["run_worker_once",
    "Advance the oldest open acquisition task by one durable stage. The service worker loop already calls this; a manual call is an operator turn, not a second worker.",
    "WorkerOnce"],
  ["get_acquisition_capabilities",
    "Read this server's implementation, runtime and governance readiness, blocking reasons, declared scope, active generation reference and contract revisions.",
    "SyncStatus"], // same input schema as SyncStatus
];

export class AcquireApi {
  /** @param {AcquireService} service
   *  @param {{ApiError?: Function}} [opts] — inject transport's
   *    ApiError at integration for `instanceof` routing. */
  constructor(service, opts = {}) {
    this.service = service;
    this.ApiError = opts.ApiError ?? DefaultApiError;
  }

  /** mcp.rs:83-120 — capabilities report; blocking reasons for no
   *  active generation + unresolved (conflict/not_verified/corrupt)
   *  generations. */
  capabilities() {
    let status;
    try {
      status = this.service.syncStatus();
    } catch (e) {
      throw this.mapError(e);
    }
    const blocking = [];
    if (status.active === null) {
      blocking.push({
        code: "no_active_generation",
        detail: "no generation has completed verify+activate; data serving from this store is blocked",
      });
    }
    for (const g of status.generations) {
      if (g.state === "conflict" || g.state === "not_verified" ||
          g.state === "corrupt") {
        blocking.push({
          code: "unresolved_generation",
          detail: `generation ${g.generation_id} is ${g.state}`,
          generation_id: g.generation_id,
        });
      }
    }
    return {
      implementation: {
        server: "x-data-acquire",
        version: PKG_VERSION,
        capability_revision: CAPABILITY_REVISION,
        generation_contract_version: GENERATION_CONTRACT_VERSION,
        consistency_contract_version: status.consistency_contract_version,
      },
      declared_scope:
        "official TDX base/gbbq/hsjday acquisition into immutable generations; no catalog scope claims",
      active_generation_ref:
        status.active === null ? null : status.active.generation_id,
      governance_readiness: "ready",
      runtime_readiness: blocking.length === 0 ? "ready" : "blocked",
      blocking_reasons: blocking,
      tools: ENTRIES.map(([name]) => name),
      // schema_digest: sha256 over serde to_vec of
      // [{name,input},...] — Value BTreeMap order: "input" < "name";
      // SCHEMAS entries are stored deep-sorted at module load.
      schema_digest: sha256hex(Buffer.from(
        serJson(ENTRIES.map(([name, , schemaKey]) =>
          ({ input: SCHEMAS[schemaKey], name }))),
        "utf8")),
    };
  }

  /** ToolHandler::tools — ENTRIES as {name,description,inputSchema}. */
  tools() {
    return ENTRIES.map(([name, description, schemaKey]) => ({
      name, description, inputSchema: SCHEMAS[schemaKey],
    }));
  }

  /** ToolHandler::call (mcp.rs:159-252) — contract_version gate,
   *  per-tool decode→service→encode, unknown_tool. Errors: decode →
   *  ApiError("invalid_arguments"); service → mapped(); both thrown as
   *  this.ApiError (inject the transport class at integration). */
  call(name, input) {
    // input.get("contract_version").is_some_and(|v| v != "v1")
    if (input !== null && typeof input === "object" &&
        !Array.isArray(input) &&
        "contract_version" in input && input.contract_version !== "v1") {
      throw new this.ApiError("unsupported_contract",
        "unsupported contract version");
    }
    const svc = this.service;
    const dec = (t) => { // decode<T> — serde error -> invalid_arguments
      try { return parseWire[t](input); }
      catch (e) {
        throw new this.ApiError("invalid_arguments", String(e.message));
      }
    };
    const run = (fn) => { // .map_err(mapped)?
      try { return fn(); } catch (e) { throw this.mapError(e); }
    };
    switch (name) {
      case "check_official_updates":
        dec("CheckOfficialUpdates");
        return run(() => svc.checkUpdates());
      case "acquire_generation": {
        const a = dec("AcquireGeneration");
        return run(() => svc.acquireGeneration(a.request_id));
      }
      case "get_acquisition_task": {
        const a = dec("GetAcquisitionTask");
        return run(() => svc.task(a.task_id));
      }
      case "get_acquisition_request": {
        const a = dec("GetAcquisitionRequest");
        return run(() => svc.request(a.request_id));
      }
      case "verify_generation": {
        const a = dec("VerifyGeneration");
        return run(() =>
          svc.verifyGeneration(a.request_id, a.generation_id));
      }
      case "activate_generation": {
        const a = dec("ActivateGeneration");
        return run(() => svc.activateGeneration(
          a.request_id, a.generation_id, a.expected_previous ?? null));
      }
      case "retire_generation": {
        const a = dec("RetireGeneration");
        return run(() => svc.retireGeneration(
          a.request_id, a.generation_id, a.confirm_token));
      }
      case "sync_status":
        dec("SyncStatus");
        return run(() => svc.syncStatus());
      case "run_worker_once": {
        dec("WorkerOnce");
        const task = run(() => svc.workerOnce());
        let view = null;
        if (task !== null) {
          try {
            view = svc.task(task.task_id); // fresh durable view
          } catch {
            view = taskView(task); // unwrap_or_else fallback (verbatim)
          }
        }
        return { idle: task === null, task: view };
      }
      case "get_acquisition_capabilities":
        dec("SyncStatus");
        return this.capabilities();
      default:
        throw new this.ApiError("unknown_tool",
          `unsupported tool: ${name}`);
    }
  }

  /** mcp.rs:128-142 mapped() — structural match over service::Error. */
  mapError(e) {
    const A = this.ApiError;
    if (e?.kind === "refused") return new A(e.code(), e.message);
    if (e?.kind === "storage") {
      return new A(
        e.inner?.kind === "request_conflict"
          ? "request_conflict" : "storage_error",
        e.message);
    }
    if (e?.kind === "generation") {
      return new A(
        e.inner?.kind === "not_installed"
          ? "generation_not_installed" : "generation_error",
        e.message);
    }
    if (e?.kind === "fetch") return new A("fetch_error", e.message);
    if (e?.kind === "json") return new A("internal_error", e.message);
    if (e?.kind === "io") return new A("io_error", e.message);
    return new A("internal_error", String(e?.message ?? e));
  }
}
