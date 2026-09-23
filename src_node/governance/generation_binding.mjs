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

// Own data-property descriptor lookup: JSON/serde values carry only
// data properties — accessor descriptors (get/set) are rejected
// WITHOUT invoking them.
const ownData = (obj, key) => {
  const d = Object.getOwnPropertyDescriptor(obj, key);
  return d === undefined ? { present: false }
    : "value" in d ? { present: true, value: d.value }
    : { present: true, accessor: true };
};
// Every own property (incl. non-enumerable) must be a data property
// with a wellformed string key — serde maps have neither accessors
// nor non-enumerable/symbol members.
const plainDataProps = (obj) =>
  Object.getOwnPropertySymbols(obj).length === 0 &&
  Object.getOwnPropertyNames(obj).every((k) => {
    const d = Object.getOwnPropertyDescriptor(obj, k);
    return "value" in d && d.enumerable && k.isWellFormed();
  });
// Deep JSON-shape check for a serde_json::Value: scalars (null/bool/
// finite number/wellformed string), arrays, and plain or null-prototype
// objects whose every own property is an enumerable data property.
// Accessor descriptors reject before any invocation.
function isJsonValue(v) {
  if (v === null) return true;
  const t = typeof v;
  if (t === "boolean") return true;
  if (t === "number") return Number.isFinite(v);
  if (t === "string") return v.isWellFormed();
  if (Array.isArray(v)) {
    // Array elements are indexed own properties — read via descriptor
    // (never v[i]/v.every, which would invoke index getters).
    if (Object.getOwnPropertySymbols(v).length !== 0) return false;
    const len = Object.getOwnPropertyDescriptor(v, "length");
    if (len === undefined || !("value" in len)) return false;
    return Object.getOwnPropertyNames(v).every((k) => {
      if (k === "length") return true;
      const d = Object.getOwnPropertyDescriptor(v, k);
      return "value" in d && d.enumerable && isJsonValue(d.value);
    });
  }
  if (t === "object") {
    if (Object.getPrototypeOf(v) !== Object.prototype &&
        Object.getPrototypeOf(v) !== null) return false;
    if (!plainDataProps(v)) return false;
    return Object.keys(v).every((k) => isJsonValue(v[k]));
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
  // An ItemSpec is a serde struct — accessor/non-enumerable/symbol own
  // properties are unrepresentable; descriptors checked, never invoked.
  if (!plainDataProps(item)) {
    throw new GenerationBindingInputError("item has non-data properties");
  }
  const fp = ownData(item, "frozen_inputs");
  if (!fp.present) {
    throw new GenerationBindingInputError("item.frozen_inputs required");
  }
  const frozen = fp.value;
  if (!isJsonValue(frozen)) {
    throw new GenerationBindingInputError("item.frozen_inputs must be a JSON value");
  }
  if (frozen === null || typeof frozen !== "object" || Array.isArray(frozen)) {
    return { kind: "missing" };
  }
  const kp = ownData(frozen, SOURCE_GENERATION_KEY); // data prop (checked above)
  if (!kp.present) return { kind: "missing" };
  return isStr(kp.value) ? { kind: "id", id: kp.value } : { kind: "null" };
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
