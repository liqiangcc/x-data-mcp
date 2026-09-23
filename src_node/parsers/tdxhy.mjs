// Bounded `tdxhy.cfg` line parser — Node port of src/catalog_tdx/tdxhy.rs.
//
// Anchored shape (contract v1, #168): six `|`-separated fields, ASCII
// only, CRLF terminators. f0 market digit 0..=2, f1 six-digit code,
// f2 `T` industry code or empty, f3/f4 reserved (must be empty), f5 `X`
// research code or empty. One malformed line fails the whole file —
// no skip, no repair, no business meaning assigned.
//
// Error texts byte-identical to TdxhyError variants, same precedence:
//   NotAscii -> per line: EmptyLine -> FieldCount -> BadMarket ->
//   BadCode -> BadIndustryCode -> NonEmptyReservedField(f3 then f4) ->
//   BadResearchCode

export class TdxhyError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "TdxhyError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

// Anchored TDX code shape: letter + 2/4/6 digits (len 3|5|7).
const codeShape = (value, letter) =>
  (value.length === 3 || value.length === 5 || value.length === 7) &&
  value.charCodeAt(0) === letter &&
  /^[0-9]+$/.test(value.slice(1));

const DIGITS6 = /^[0-9]{6}$/;

/**
 * @param {Buffer|Uint8Array} bytes — typed boundary, no coercion
 * @returns {{line:number,market:number,code:string,
 *            t_code:string|null,x_code:string|null}[]}
 */
export function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new TdxhyError("tdxhy.cfg input must be a Buffer or Uint8Array", "invalid_input");
  }
  for (let i = 0; i < bytes.length; i++) {
    if (bytes[i] > 0x7f) {
      throw new TdxhyError("tdxhy.cfg is not pure ASCII", "not_ascii");
    }
  }
  const text = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString("latin1");
  const lines = text.split("\r\n");
  const rows = [];
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    const number = index + 1;
    if (line === "") {
      if (index === lines.length - 1) break; // single trailing CRLF tolerated
      throw new TdxhyError(`tdxhy.cfg line ${number} is empty`, "empty_line", { line: number });
    }
    const fields = line.split("|");
    if (fields.length !== 6) {
      throw new TdxhyError(
        `tdxhy.cfg line ${number} has ${fields.length} fields, expected 6`,
        "field_count", { fields: fields.length, line: number });
    }
    const market = fields[0] === "0" ? 0 : fields[0] === "1" ? 1 : fields[0] === "2" ? 2 : null;
    if (market === null) {
      throw new TdxhyError(
        `tdxhy.cfg line ${number} f0 "${fields[0]}" is not a single digit 0..=2`,
        "bad_market", { line: number, value: fields[0] });
    }
    const code = fields[1];
    if (!DIGITS6.test(code)) {
      throw new TdxhyError(
        `tdxhy.cfg line ${number} f1 "${code}" is not exactly six digits`,
        "bad_code", { line: number, value: code });
    }
    const t = fields[2];
    const tCode = t === "" ? null : codeShape(t, 0x54) ? t : (() => {
      throw new TdxhyError(
        `tdxhy.cfg line ${number} f2 "${t}" is not a T code of anchored length`,
        "bad_industry_code", { line: number, value: t });
    })();
    for (const [fi, value] of [[3, fields[3]], [4, fields[4]]]) {
      if (value !== "") {
        throw new TdxhyError(
          `tdxhy.cfg line ${number} reserved field f${fi} must be empty`,
          "non_empty_reserved_field", { index: fi, line: number });
      }
    }
    const x = fields[5];
    const xCode = x === "" ? null : codeShape(x, 0x58) ? x : (() => {
      throw new TdxhyError(
        `tdxhy.cfg line ${number} f5 "${x}" is not an X code of anchored length`,
        "bad_research_code", { line: number, value: x });
    })();
    rows.push({ code, line: number, market, t_code: tCode, x_code: xCode });
  }
  return rows;
}
