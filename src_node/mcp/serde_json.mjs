// serde_json::Value-compatible JSON parse/serialize for MCP wire parity.
//
// parseJsonBytes(buf) reproduces serde_json::from_str::<Value> semantics:
//   - error strings verbatim: "<kind> at line 1 column <n>" where column is
//     BYTES consumed (non-EOF errors consume the offending byte; EOF errors
//     report the bytes consumed so far). Input is one transport line, so the
//     line component is always 1.
//   - numbers: integer lexemes fitting i64/u64 -> safe Number or BigInt;
//     out-of-range integers and all float lexemes -> F64 wrapper (floatness
//     preserved for verbatim re-serialization, e.g. `1.0` -> "1.0",
//     `-0` -> "-0.0", `1e300` -> "1e+300"); overflow to infinity ->
//     "number out of range"; underflow -> 0.0.
//   - duplicate keys: last wins; 128-deep recursion limit.
//
// serValue(v) reproduces serde_json::to_string(&Value): object keys sorted
// in UTF-8 byte order (BTreeMap), f64 via ryu::format_finite notation
// (fixed for decimal exponent -5..=15, scientific otherwise, ".0" suffix
// on integral floats), strings escaped like serde (C0 controls -> \u00XX).
//
// Used by mcp/transport.mjs. Handlers never see F64 (transport unwraps
// arguments); F64 exists so request `id` values echo byte-exactly.

export class F64 {
  constructor(v) {
    this.v = v;
  }
}

const WS = new Set([0x20, 0x09, 0x0a, 0x0d]);
const DIGIT = (b) => b >= 0x30 && b <= 0x39;
const I64_MIN = -(1n << 63n);
const U64_MAX = (1n << 64n) - 1n;
const RECURSION_LIMIT = 128;

class Parse {
  constructor(buf) {
    this.buf = buf;
    this.pos = 0;
    this.depth = 0;
  }

  // Error column = bytes consumed. `at(idx)` reports after consuming the
  // offending byte; `eof()` reports at the current (end) position.
  at(idx, kind) {
    this.pos = idx;
    return { ok: false, message: `${kind} at line 1 column ${this.pos}` };
  }
  eof(kind) {
    return {
      ok: false,
      eofKind: kind,
      message: `EOF while parsing ${kind} at line 1 column ${this.pos}`,
    };
  }
  peek() {
    return this.pos < this.buf.length ? this.buf[this.pos] : -1;
  }
  next() {
    return this.pos < this.buf.length ? this.buf[this.pos++] : -1;
  }
  skipWs() {
    while (this.pos < this.buf.length && WS.has(this.buf[this.pos])) this.pos++;
  }

  run() {
    this.skipWs();
    const v = this.value();
    if (!v.ok) return v;
    this.skipWs();
    if (this.pos < this.buf.length) return this.at(this.pos + 1, "trailing characters");
    return { ok: true, value: v.value };
  }

  value() {
    const b = this.peek();
    if (b < 0) return this.eof("a value");
    switch (b) {
      case 0x7b: // {
        return this.container(true);
      case 0x5b: // [
        return this.container(false);
      case 0x22: {
        // "
        const s = this.string();
        if (!s.ok) return s;
        return { ok: true, value: s.value };
      }
      case 0x74:
        return this.ident("rue", true);
      case 0x66:
        return this.ident("alse", false);
      case 0x6e:
        return this.ident("ull", null);
      case 0x2d: // -
      default:
        if (b === 0x2d || DIGIT(b)) return this.number();
        return this.at(this.pos + 1, "expected value");
    }
  }

  ident(rest, value) {
    this.pos++; // consume first byte already matched by caller
    for (const ch of rest) {
      const b = this.next();
      if (b < 0) return this.eof("a value");
      if (b !== ch.charCodeAt(0)) return this.at(this.pos, "expected ident");
    }
    return { ok: true, value };
  }

  container(isObj) {
    this.pos++; // consume { or [
    this.depth++;
    if (this.depth > RECURSION_LIMIT - 1)
      return this.at(this.pos, "recursion limit exceeded");
    const kind = isObj ? "an object" : "a list";
    const close = isObj ? 0x7d : 0x5d; // } or ]
    const commaErr = isObj ? "expected `,` or `}`" : "expected `,` or `]`";
    this.skipWs();
    const out = isObj ? {} : [];
    let first = true;
    for (;;) {
      let b = this.peek();
      if (b < 0) return this.eof(kind);
      if (!first) {
        // position after element: only , or close is legal
        if (b === close) {
          this.pos++;
          this.depth--;
          return { ok: true, value: out };
        }
        if (b !== 0x2c) return this.at(this.pos + 1, commaErr);
        this.pos++;
        this.skipWs();
        const c = this.peek();
        if (c === close) return this.at(this.pos + 1, "trailing comma");
        // EOF here lands in the element/key context below
      } else if (b === close) {
        this.pos++;
        this.depth--;
        return { ok: true, value: out };
      }
      first = false;
      if (isObj) {
        const k = this.peek();
        // serde: post-comma key position EOF is a value-context error; the
        // first-element EOF was already reported as "an object" above.
        if (k < 0) return this.eof("a value");
        if (k !== 0x22) return this.at(this.pos + 1, "key must be a string");
        const key = this.string();
        // serde maps EOF inside an object key to the object context
        if (!key.ok) return key.eofKind === "a string" ? this.eof(kind) : key;
        this.skipWs();
        const colon = this.next();
        if (colon < 0) return this.eof(kind);
        if (colon !== 0x3a) return this.at(this.pos, "expected `:`");
        this.skipWs();
        const v = this.value();
        if (!v.ok) return v;
        out[key.value] = v.value;
      } else {
        const v = this.value();
        if (!v.ok) return v;
        out.push(v.value);
      }
      this.skipWs();
    }
  }

  string() {
    // on entry pos points at the opening quote
    this.pos++;
    const parts = [];
    let start = this.pos;
    const flush = (end) => {
      if (end > start) parts.push(this.buf.toString("utf8", start, end));
    };
    for (;;) {
      const b = this.next();
      if (b < 0) return this.eof("a string");
      if (b === 0x22) {
        flush(this.pos - 1);
        return { ok: true, value: parts.join("") };
      }
      if (b < 0x20)
        return this.at(this.pos, "control character (\\u0000-\\u001F) found while parsing a string");
      if (b !== 0x5c) continue;
      flush(this.pos - 1); // bytes before the backslash
      const e = this.next();
      if (e < 0) return this.eof("a string");
      switch (e) {
        case 0x22:
          parts.push('"');
          break;
        case 0x5c:
          parts.push("\\");
          break;
        case 0x2f:
          parts.push("/");
          break;
        case 0x62:
          parts.push("\b");
          break;
        case 0x66:
          parts.push("\f");
          break;
        case 0x6e:
          parts.push("\n");
          break;
        case 0x72:
          parts.push("\r");
          break;
        case 0x74:
          parts.push("\t");
          break;
        case 0x75: {
          const u = this.hexEscape();
          if (!u.ok) return u;
          parts.push(u.value);
          break;
        }
        default:
          return this.at(this.pos, "invalid escape");
      }
      start = this.pos;
    }
  }

  hex4() {
    // serde_json consumes exactly 4 bytes after \u then validates
    let v = 0;
    for (let i = 0; i < 4; i++) {
      const b = this.next();
      if (b < 0) return { ok: false, eof: true };
      const d =
        b >= 0x30 && b <= 0x39 ? b - 0x30 :
        b >= 0x61 && b <= 0x66 ? b - 0x61 + 10 :
        b >= 0x41 && b <= 0x46 ? b - 0x41 + 10 : -1;
      if (d < 0) {
        // consume the remaining digits like serde's 4-byte read
        while (++i < 4) {
          if (this.next() < 0) return { ok: false, eof: true };
        }
        return { ok: false, eof: false };
      }
      v = v * 16 + d;
    }
    return { ok: true, value: v };
  }

  hexEscape() {
    const u = this.hex4();
    if (!u.ok) {
      if (u.eof) return this.eof("a string");
      return this.at(this.pos, "invalid escape");
    }
    let cp = u.value;
    if (cp >= 0xd800 && cp <= 0xdbff) {
      // high surrogate: serde expects a second \uXXXX low surrogate
      const b = this.next();
      if (b < 0) return this.eof("a string");
      if (b !== 0x5c) return this.at(this.pos, "unexpected end of hex escape");
      const u2 = this.next();
      if (u2 < 0) return this.eof("a string");
      if (u2 !== 0x75) return this.at(this.pos, "unexpected end of hex escape");
      const lo = this.hex4();
      if (!lo.ok) {
        if (lo.eof) return this.eof("a string");
        return this.at(this.pos, "invalid escape");
      }
      if (lo.value < 0xdc00 || lo.value > 0xdfff)
        return this.at(this.pos, "lone leading surrogate in hex escape");
      cp = 0x10000 + ((cp - 0xd800) << 10) + (lo.value - 0xdc00);
      return { ok: true, value: String.fromCodePoint(cp) };
    }
    if (cp >= 0xdc00 && cp <= 0xdfff)
      return this.at(this.pos, "lone leading surrogate in hex escape");
    return { ok: true, value: String.fromCharCode(cp) };
  }

  number() {
    const start = this.pos;
    let neg = false;
    if (this.peek() === 0x2d) {
      this.pos++;
      neg = true;
    }
    let b = this.peek();
    if (b < 0) return this.eof("a value");
    if (!DIGIT(b)) return this.at(this.pos + 1, "invalid number");
    let isFloat = false;
    if (b === 0x30) {
      this.pos++;
      const n = this.peek();
      if (n >= 0 && DIGIT(n)) return this.at(this.pos + 1, "invalid number");
    } else {
      while (DIGIT(this.peek())) this.pos++;
    }
    if (this.peek() === 0x2e) {
      // '.'
      isFloat = true;
      this.pos++;
      b = this.peek();
      if (b < 0) return this.eof("a value");
      if (!DIGIT(b)) return this.at(this.pos + 1, "invalid number");
      while (DIGIT(this.peek())) this.pos++;
    }
    if (this.peek() === 0x65 || this.peek() === 0x45) {
      // e/E
      isFloat = true;
      this.pos++;
      const s = this.peek();
      if (s === 0x2b || s === 0x2d) this.pos++;
      b = this.peek();
      if (b < 0) return this.eof("a value");
      if (!DIGIT(b)) return this.at(this.pos + 1, "invalid number");
      while (DIGIT(this.peek())) this.pos++;
    }
    const lexeme = this.buf.toString("ascii", start, this.pos);
    if (!isFloat) {
      if (neg && lexeme === "-0") return { ok: true, value: new F64(-0) };
      const big = BigInt(lexeme);
      if (big >= I64_MIN && big <= U64_MAX) {
        if (big >= BigInt(Number.MIN_SAFE_INTEGER) && big <= BigInt(Number.MAX_SAFE_INTEGER))
          return { ok: true, value: Number(big) };
        return { ok: true, value: big };
      }
      return { ok: true, value: new F64(Number(lexeme)) }; // integer overflow -> f64
    }
    const v = Number(lexeme);
    if (!Number.isFinite(v)) return this.at(this.pos, "number out of range");
    return { ok: true, value: new F64(v) };
  }
}

// Parse one transport line. Accepts Buffer (preferred) or string.
export function parseJsonBytes(input) {
  const buf = Buffer.isBuffer(input) ? input : Buffer.from(input, "utf8");
  return new Parse(buf).run();
}

// serde_json float formatting (ryu::format_finite): fixed notation for
// decimal exponent -5..=15, scientific otherwise; integral floats gain ".0".
export function serF64(v) {
  if (!Number.isFinite(v)) throw new Error("non-finite number cannot serialize");
  if (v === 0) return Object.is(v, -0) ? "-0.0" : "0.0";
  const sign = v < 0 ? "-" : "";
  const s = String(Math.abs(v));
  let digits, e10;
  const ei = s.indexOf("e");
  if (ei >= 0) {
    const m = s.slice(0, ei);
    const exp = parseInt(s.slice(ei + 1), 10);
    // JS scientific mantissa is always normalized: one digit before the point
    digits = m.replace(".", "");
    e10 = exp;
  } else {
    const dot = s.indexOf(".");
    if (dot < 0) {
      digits = s.replace(/0+$/, "") || "0";
      e10 = s.length - 1;
    } else {
      const intPart = s.slice(0, dot);
      const frac = s.slice(dot + 1);
      if (intPart !== "0") {
        digits = intPart + frac;
        e10 = intPart.length - 1;
      } else {
        const lead = frac.match(/^0*/)[0].length;
        digits = frac.slice(lead);
        e10 = -(lead + 1);
      }
      digits = digits.replace(/0+$/, "");
    }
  }
  if (e10 >= -5 && e10 <= 15) {
    let out;
    if (e10 >= digits.length - 1) {
      out = digits + "0".repeat(e10 - digits.length + 1) + ".0";
    } else if (e10 >= 0) {
      out = digits.slice(0, e10 + 1) + "." + digits.slice(e10 + 1);
    } else {
      out = "0." + "0".repeat(-e10 - 1) + digits;
    }
    return sign + out;
  }
  const mant = digits.length > 1 ? digits[0] + "." + digits.slice(1) : digits;
  return sign + mant + "e" + (e10 < 0 ? "-" : "+") + Math.abs(e10);
}

const keyCmp = (a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));

// serde_json::to_string(&Value) parity: BTreeMap-sorted keys, compact.
export function serValue(v) {
  if (v === null || v === undefined) return "null";
  if (v instanceof F64) return serF64(v.v);
  if (typeof v === "bigint") return v.toString();
  if (typeof v === "number") return Number.isInteger(v) ? String(v) : serF64(v);
  if (typeof v === "string") return JSON.stringify(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (typeof v.toJSON === "function") return serValue(v.toJSON());
  if (Array.isArray(v)) return `[${v.map(serValue).join(",")}]`;
  const keys = Object.keys(v).sort(keyCmp);
  return `{${keys.map((k) => `${JSON.stringify(k)}:${serValue(v[k])}`).join(",")}}`;
}
