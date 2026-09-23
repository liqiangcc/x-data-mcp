// Catalog scope partition — Node port of src/catalog_tdx/scope.rs.
//
// S = { market ∈ {0,1} ∧ t_code ≠ ε ∧ x_code ≠ ε }.
// Every non-S row is classified into exactly one exclusion class:
//   market == 2 -> BjMarket (regardless of codes)
//   market 0/1 + missing T or X + code prefix 200/900 -> BShare
//   remaining missing-code rows -> Anomalous
// Disjoint and exhaustive by construction; input order preserved;
// labels carry no business meaning beyond the mechanical rule.

export class ScopeBoundaryError extends Error {
  constructor(message) {
    super(message);
    this.name = "ScopeBoundaryError";
    this.kind = "typed_boundary";
  }
}

const DIGITS6 = /^[0-9]{6}$/;
const isLine = (v) => Number.isInteger(v) && v >= 1;
const isCodeStr = (v) => typeof v === "string" && v !== "";
const isOptCode = (v) => v === null || isCodeStr(v);

const validateRow = (row, i) => {
  if (row === null || typeof row !== "object" || Array.isArray(row)) {
    throw new ScopeBoundaryError(`rows[${i}] must be a TdxhyRow-shaped object`);
  }
  if (!isLine(row.line)) throw new ScopeBoundaryError(`rows[${i}].line must be a positive integer`);
  if (!Number.isInteger(row.market) || row.market < 0 || row.market > 2) {
    throw new ScopeBoundaryError(`rows[${i}].market must be an integer 0..2`);
  }
  if (!DIGITS6.test(row.code)) {
    throw new ScopeBoundaryError(`rows[${i}].code must be a six-digit string`);
  }
  if (!isOptCode(row.t_code)) throw new ScopeBoundaryError(`rows[${i}].t_code must be a string or null`);
  if (!isOptCode(row.x_code)) throw new ScopeBoundaryError(`rows[${i}].x_code must be a string or null`);
};

/**
 * Partition parsed TdxhyRow[] into S members and classified exclusions.
 * @param {{line:number,market:number,code:string,
 *          t_code:string|null,x_code:string|null}[]} rows
 * @returns {{members:{line,market,code,t_code,x_code}[],
 *            excluded:{line,market,code,class}[]}}
 */
export function apply(rows) {
  if (!Array.isArray(rows)) {
    throw new ScopeBoundaryError("rows must be an array");
  }
  const members = [];
  const excluded = [];
  rows.forEach((row, i) => {
    validateRow(row, i);
    if (row.market === 2) {
      excluded.push({ class: "BjMarket", code: row.code, line: row.line, market: row.market });
      return;
    }
    if (row.t_code !== null && row.x_code !== null) {
      members.push({
        code: row.code, line: row.line, market: row.market,
        t_code: row.t_code, x_code: row.x_code,
      });
      return;
    }
    excluded.push({
      class: row.code.startsWith("200") || row.code.startsWith("900")
        ? "BShare" : "Anomalous",
      code: row.code, line: row.line, market: row.market,
    });
  });
  return { excluded, members };
}

/**
 * Row-count identity the refresh diagnostics must report.
 * @returns {{total:number,scope:number,bj_market:number,
 *            b_share:number,anomalous:number}}
 */
export function reconciliation(outcome, total) {
  if (!Number.isInteger(total) || total < 0) {
    throw new ScopeBoundaryError("total must be a non-negative integer");
  }
  const counts = {
    anomalous: outcome.excluded.filter((r) => r.class === "Anomalous").length,
    b_share: outcome.excluded.filter((r) => r.class === "BShare").length,
    bj_market: outcome.excluded.filter((r) => r.class === "BjMarket").length,
    scope: outcome.members.length,
    total,
  };
  const sum = counts.scope + counts.bj_market + counts.b_share + counts.anomalous;
  if (sum !== total) {
    throw new ScopeBoundaryError(
      `scope partition broken: ${sum} classified of ${total} rows`);
  }
  return counts;
}
