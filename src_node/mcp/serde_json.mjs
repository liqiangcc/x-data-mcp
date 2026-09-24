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

// Object-key order: serde_json BTreeMap = UTF-8 byte order == code
// point order (UTF-8 is order-preserving). Code-unit compare is wrong
// for supplementary-plane keys (a surrogate pair's lead unit sits in
// 0xD800-0xDFFF, below the BMP range its code point exceeds), so when
// either side holds a surrogate we compare by code point.
const keyCmp = (a, b) => {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const ca = a.charCodeAt(i), cb = b.charCodeAt(i);
    if (ca === cb) continue;
    if ((ca & 0xf800) === 0xd800 || (cb & 0xf800) === 0xd800) {
      return a.codePointAt(i) - b.codePointAt(i);
    }
    return ca - cb;
  }
  return a.length - b.length;
};

// serde_json string escape policy — identical to JSON.stringify:
// \b \t \n \f \r \" \\ plus C0 < 0x20 as \u00xx (lowercase hex);
// DEL and everything >= 0x20 passes through raw (UTF-8 encoded).
const ESCAPE = new Array(128);
for (let i = 0; i < 0x20; i++) {
  ESCAPE[i] = "\\u" + i.toString(16).padStart(4, "0");
}
ESCAPE[8] = "\\b"; ESCAPE[9] = "\\t"; ESCAPE[10] = "\\n";
ESCAPE[12] = "\\f"; ESCAPE[13] = "\\r";
ESCAPE[34] = '\\"'; ESCAPE[92] = "\\\\";
const NEEDS_ESCAPE = new Uint8Array(128);
for (let i = 0; i < 0x20; i++) NEEDS_ESCAPE[i] = 1;
NEEDS_ESCAPE[34] = 1; NEEDS_ESCAPE[92] = 1;

// Byte-level emit: a single growable buffer, no per-leaf string
// allocation. Strings are scanned once for escapable chars; clean
// spans go through one Buffer.utf8 write.
class ByteWriter {
  constructor() {
    this.b = Buffer.allocUnsafe(1 << 16);
    this.p = 0;
  }
  need(n) {
    if (this.p + n <= this.b.length) return;
    const nb = Buffer.allocUnsafe(
      Math.max(this.b.length * 2, this.b.length + n));
    this.b.copy(nb, 0, 0, this.p);
    this.b = nb;
  }
  byte(c) {
    if (this.p >= this.b.length) this.need(8);
    this.b[this.p++] = c;
  }
  lit(s) { // ASCII-only literal
    const n = s.length;
    this.need(n);
    for (let i = 0; i < n; i++) this.b[this.p++] = s.charCodeAt(i);
  }
  utf8(s) {
    this.need(s.length * 3 + 2);
    this.p += this.b.write(s, this.p, "utf8");
  }
  str(s) { // quoted + escaped
    this.byte(34);
    let start = 0;
    const n = s.length;
    for (let i = 0; i < n; i++) {
      const c = s.charCodeAt(i);
      if (c < 0x80) {
        if (NEEDS_ESCAPE[c]) {
          if (i > start) this.utf8(s.slice(start, i));
          this.lit(ESCAPE[c]);
          start = i + 1;
        }
        continue;
      }
      // Lone surrogates: JSON.stringify emits \udXXX (lowercase);
      // utf8Write would emit U+FFFD. Valid pairs pass through raw.
      if (c >= 0xd800 && c <= 0xdbff &&
          (i + 1 >= n || !(s.charCodeAt(i + 1) >= 0xdc00 && s.charCodeAt(i + 1) <= 0xdfff))) {
        if (i > start) this.utf8(s.slice(start, i));
        this.lit("\\u" + c.toString(16).padStart(4, "0"));
        start = i + 1;
      } else if (c >= 0xdc00 && c <= 0xdfff) {
        if (i > start) this.utf8(s.slice(start, i));
        this.lit("\\u" + c.toString(16).padStart(4, "0"));
        start = i + 1;
      } else if (c >= 0xd800 && c <= 0xdbff) {
        i++; // valid pair — consume the trail too
      }
    }
    if (start === 0) this.utf8(s);
    else if (start < n) this.utf8(s.slice(start));
    this.byte(34);
  }
  bytes() { return this.b.subarray(0, this.p); }
  text() { return this.b.toString("utf8", 0, this.p); }
}

const serInto = (v, w) => {
  if (v === null || v === undefined) { w.lit("null"); return; }
  const t = typeof v;
  if (t === "string") { w.str(v); return; }
  if (t === "number") {
    w.lit(Number.isInteger(v) ? String(v) : serF64(v));
    return;
  }
  if (t === "boolean") { w.lit(v ? "true" : "false"); return; }
  if (t === "bigint") { w.lit(v.toString()); return; }
  if (v instanceof F64) { w.lit(serF64(v.v)); return; }
  if (typeof v.toJSON === "function") { serInto(v.toJSON(), w); return; }
  if (Array.isArray(v)) {
    w.byte(91);
    for (let i = 0; i < v.length; i++) {
      if (i) w.byte(44);
      serInto(v[i], w);
    }
    w.byte(93);
    return;
  }
  w.byte(123);
  const keys = Object.keys(v);
  if (keys.length > 1) keys.sort(keyCmp);
  for (let i = 0; i < keys.length; i++) {
    if (i) w.byte(44);
    w.str(keys[i]);
    w.byte(58);
    serInto(v[keys[i]], w);
  }
  w.byte(125);
};

// serde_json::to_string(&Value) parity: BTreeMap-sorted keys, compact.
// Byte-level emit (NM-090): the returned Buffer is a live view over
// the writer's scratch — copy before storing if the bytes must
// outlive the caller's own writes.
export function serValueBytes(v) {
  const w = new ByteWriter();
  serInto(v, w);
  return w.bytes();
}
export function serValue(v) {
  const w = new ByteWriter();
  serInto(v, w);
  return w.text();
}
