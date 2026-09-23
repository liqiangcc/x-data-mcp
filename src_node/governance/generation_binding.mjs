// generation binding — Node port of
// data_supply/generation.rs::{binding_of, uniform_binding} (pure
// frozen-generation binding decision). Resolves source_generation_id
// from a task's frozen item inputs exactly once; missing/null/mixed/no
// items are refusals — never a fallback.
//
// serde shape notes:
//   - ItemSpec.frozen_inputs is serde_json::Value; Value::get() on a
//     non-object returns None -> binding = Missing (not a type error).
//   - present non-string value -> Some(None) -> "Null" refusal.
//   - Ok("") is reachable (empty string is a uniform binding); whether
//     it is usable is Store::usable's separate later gate — this pure
//     contract is preserved unchanged.
//
// Boundary: items must be an array of objects carrying an own-property
// `frozen_inputs` (serde requires the field). Binding lookup uses
// Object.hasOwn — inherited members (constructor/__proto__) can never
// manufacture a binding. Unpaired surrogates reject.
//
// Pure/read-only: no active pointer, fallback root, generation open,
// file pin, storage, or source-usability claim.

export const SOURCE_GENERATION_KEY = "source_generation_id";

export class GenerationBindingInputError extends Error {
  constructor(message) { super(message); this.name = "GenerationBindingInputError"; }
}

const isStr = (v) => typeof v === "string" && v.isWellFormed();

// Deep JSON-shape check for a serde_json::Value: scalars (null/bool/
// finite number/string), arrays, and null-prototype-equivalent objects
// with string keys. Getters/prototype members are ignored — the check
// reads only own enumerable data properties.
function isJsonValue(v) {
  if (v === null) return true;
  const t = typeof v;
  if (t === "boolean") return true;
  if (t === "number") return Number.isFinite(v);
  if (t === "string") return v.isWellFormed();
  if (Array.isArray(v)) return v.every(isJsonValue);
  if (t === "object") {
    if (Object.getPrototypeOf(v) !== Object.prototype &&
        Object.getPrototypeOf(v) !== null) return false;
    return Object.keys(v).every((k) => k.isWellFormed() && isJsonValue(v[k]));
  }
  return false;
}

// binding_of(item): absent key or non-object frozen_inputs -> None;
// present non-string -> Some(None); present string -> Some(Some(s)).
// Returns one of: {kind:"missing"} | {kind:"null"} | {kind:"id", id}
function bindingOf(item) {
  if (item === null || typeof item !== "object" || Array.isArray(item)) {
    throw new GenerationBindingInputError("item must be an object");
  }
  if (!Object.hasOwn(item, "frozen_inputs")) {
    throw new GenerationBindingInputError("item.frozen_inputs required");
  }
  const frozen = item.frozen_inputs;
  if (!isJsonValue(frozen)) {
    throw new GenerationBindingInputError("item.frozen_inputs must be a JSON value");
  }
  if (frozen === null || typeof frozen !== "object" || Array.isArray(frozen)) {
    return { kind: "missing" };
  }
  if (!Object.hasOwn(frozen, SOURCE_GENERATION_KEY)) return { kind: "missing" };
  const v = frozen[SOURCE_GENERATION_KEY];
  return isStr(v) ? { kind: "id", id: v } : { kind: "null" };
}

const sameBinding = (a, b) =>
  a.kind === b.kind && (a.kind !== "id" || a.id === b.id);

/**
 * uniform_binding(items) -> {ok:true, generation_id} |
 *   {ok:false, refusal:"NoItems"|"Missing"|"Null"|"Mixed"}
 */
export function uniformBinding(items) {
  if (!Array.isArray(items)) {
    throw new GenerationBindingInputError("items must be an array");
  }
  const it = items[Symbol.iterator]();
  const first = it.next();
  if (first.done) return { ok: false, refusal: "NoItems" };
  const firstBinding = bindingOf(first.value);
  for (const item of it) {
    if (!sameBinding(bindingOf(item), firstBinding)) {
      return { ok: false, refusal: "Mixed" };
    }
  }
  if (firstBinding.kind === "missing") return { ok: false, refusal: "Missing" };
  if (firstBinding.kind === "null") return { ok: false, refusal: "Null" };
  return { ok: true, generation_id: firstBinding.id };
}
