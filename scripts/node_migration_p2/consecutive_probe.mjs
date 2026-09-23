// NM-030 P2: validate algorithms/consecutive.mjs against a live Rust
// oracle (full Outcome serde + INVALID_MARKET_COMMAND errors +
// ratio_change strings). Exit nonzero on FAIL.
// Usage: node consecutive_probe.mjs <root> [selftest]
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { evaluate, ratioChange, ConsecutiveError, ConsecutiveInputError } from "../../src_node/algorithms/consecutive.mjs";

const ROOT = process.argv[2] || ".";
const SELFTEST = process.argv[3] || null;
const oracle = readFileSync(join(ROOT, "evidence/node-migration/p2/consecutive/rust-oracle.jsonl"), "utf8")
  .split("\n").filter(Boolean).map(JSON.parse);

let failures = 0;
const emit = (id, status, detail, expected = null, actual = null) => {
  if (status === "FAIL") failures++;
  console.log(JSON.stringify({ family: "consecutive", id, status, detail, expected, actual }));
};

const C = "complete", IC = "incomplete", UK = "unknown";
const obs = (year, close, unfinished = false, completion = C) =>
  ({ year, first: `${String(year).padStart(4, "0")}-01-02`, last: `${String(year).padStart(4, "0")}-12-30`, close, unfinished, completion });
const obsr = (year, f, l, close) =>
  ({ year, first: `${String(year).padStart(4, "0")}-${f}`, last: `${String(year).padStart(4, "0")}-${l}`, close, unfinished: false, completion: C });
const params = (direction, min, completed_only = true) => ({ direction, min_periods: min, completed_only });
const R = (a, b) => ({ start_year: a, end_year: b });

const CASES = {
  up3: () => [[obs(2020,"70"),obs(2021,"80"),obs(2022,"90"),obs(2023,"100")], R(2020,2023), params("up",3)],
  down3_lb: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"80"),obs(2023,"70")], R(2020,2023), params("down",3)],
  down2_lb_null: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"80")], R(2020,2022), params("down",3)],
  down3_exact: () => [[obs(2020,"80"),obs(2021,"100"),obs(2022,"90"),obs(2023,"80"),obs(2024,"70")], R(2020,2024), params("down",3)],
  flat_break: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"90"),obs(2023,"80")], R(2020,2023), params("down",3)],
  reverse_break: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"95")], R(2020,2022), params("down",3)],
  flat_exact_dec: () => [[obs(2020,"1.000000000000000001"),obs(2021,"1.0000000000000000010")], R(2020,2021), params("down",3)],
  missing_last_year: () => [[obs(2020,"100"),obs(2021,"90")], R(2020,2022), params("down",1)],
  missing_middle: () => [[obs(2020,"100"),obs(2022,"80"),obs(2023,"70")], R(2020,2023), params("down",1)],
  missing_left_boundary: () => [[obs(2021,"100"),obs(2022,"90"),obs(2023,"80")], R(2020,2023), params("down",1)],
  unknown_completion: () => [[obs(2020,"100",false,UK),obs(2021,"90"),obs(2022,"80")], R(2020,2022), params("down",1)],
  incomplete_middle: () => [[obs(2020,"100"),obs(2021,"90",false,IC),obs(2022,"80")], R(2020,2022), params("down",1)],
  incomplete_end_completed_only: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"80",true,IC)], R(2020,2022), params("down",1)],
  incomplete_end_provisional: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"80",true,IC)], R(2020,2022), params("down",1,false)],
  excluded_tail_two: () => [[obs(2020,"100"),obs(2021,"90"),obs(2022,"80",true,IC),obs(2023,"70",true,IC)], R(2020,2023), params("down",1)],
  unfinished_mid_completed: () => [[obs(2020,"100"),obs(2021,"90",true,C),obs(2022,"80")], R(2020,2022), params("down",1)],
  short_after_exclusion: () => [[obs(2022,"80",true,IC)], R(2022,2022), params("down",1)],
  nonpositive_zero: () => [[obs(2020,"100"),obs(2021,"0")], R(2020,2021), params("down",1)],
  nonpositive_neg: () => [[obs(2020,"100"),obs(2021,"-5")], R(2020,2021), params("down",1)],
  err_unordered: () => [[obs(2021,"90"),obs(2020,"100")], R(2020,2021), params("down",1)],
  err_dup_year: () => [[obs(2020,"100"),obs(2020,"90")], R(2020,2020), params("down",1)],
  err_out_of_range: () => [[obs(2019,"100"),obs(2020,"90")], R(2020,2020), params("down",1)],
  err_1000yr: () => [[], R(1,1001), params("down",1)],
  err_start_gt_end: () => [[], R(2022,2020), params("down",1)],
  err_start_zero: () => [[], R(0,10), params("down",1)],
  err_end_10000: () => [[], R(9990,10000), params("down",1)],
  err_min_periods_0: () => [[obs(2020,"1")], R(2020,2020), params("down",0)],
  err_min_periods_1001: () => [[obs(2020,"1")], R(2020,2020), params("down",1001)],
  err_obs_1001: () => [Array.from({length:1001},(_,i)=>obs(2020+i,"1")), R(2020,3020), params("down",1)],
  err_date_mismatch_year: () => [[{year:2020,first:"2019-06-01",last:"2020-12-30",close:"1",unfinished:false,completion:C},obs(2021,"0.5")], R(2020,2021), params("down",1)],
  err_last_next_year: () => [[{year:2020,first:"2020-01-02",last:"2021-01-02",close:"1",unfinished:false,completion:C},obs(2021,"0.5")], R(2020,2021), params("down",1)],
  err_first_gt_last: () => [[obsr(2020,"12-30","01-02","1"),obs(2021,"0.5")], R(2020,2021), params("down",1)],
  low_years: () => [[obs(1,"100"),obs(2,"90")], R(1,2), params("down",1)],
  low_years_leap: () => [[obsr(4,"02-29","12-30","100"),obsr(5,"01-02","12-30","90")], R(4,5), params("down",1)],
  year_400_leap: () => [[obsr(400,"02-29","12-30","100"),obsr(401,"01-02","12-30","90")], R(400,401), params("down",1)],
  century_2000_leap: () => [[obsr(2000,"02-29","12-30","100"),obsr(2001,"01-02","12-30","90")], R(2000,2001), params("down",1)],
  real_dates: () => [[obsr(2020,"01-02","12-31","100"),obsr(2021,"01-04","12-30","90"),obsr(2022,"01-03","12-29","80")], R(2020,2022), params("down",1)],
};
const RC = {
  rc_half_even_05: ["1.0000000000005","1"], rc_half_even_15: ["1.0000000000015","1"],
  rc_half_even_neg05: ["0.9999999999995","1"], rc_half_even_neg15: ["0.9999999999985","1"],
  rc_third: ["2","3"], rc_max: ["79228162514264337593543950335","0.0000000000000000000000000001"],
  rc_repeat: ["1","3"], rc_leadzero: ["0.001","0.01"], rc_trailzero: ["1.000","2.000"],
  rc_neg: ["50","100"], rc_scale_mix: ["1.5","2.50"],
  rc_max_exact: ["79228162514264337593543950335","79228162514264337593543950335"],
  rc_err_zero: ["1","0"], rc_err_neg: ["1","-1"], rc_err_end_neg: ["-1","1"],
};

const canon = (v) => v === null || typeof v !== "object" ? JSON.stringify(v)
  : Array.isArray(v) ? "[" + v.map(canon).join(",") + "]"
  : "{" + Object.keys(v).sort().map((k) => JSON.stringify(k) + ":" + canon(v[k])).join(",") + "}";

for (const o of oracle) {
  if (o.id.startsWith("rc_")) {
    const [e, a] = RC[o.id];
    try {
      const got = ratioChange(e, a);
      const ok = o.status === "ok" && got === o.ratio;
      emit(o.id, ok ? "PASS" : "FAIL", ok ? got : "mismatch", o.ratio ?? o.error, got);
    } catch (e) {
      const ok = o.status === "error" && e.message === o.error;
      emit(o.id, ok ? "PASS" : "FAIL", ok ? "err verbatim" : e.message, o.error, e.message);
    }
    continue;
  }
  const [input, range, p] = CASES[o.id]();
  try {
    const got = evaluate(input, range, p);
    if (o.status === "error") { emit(o.id, "FAIL", "accepted", o.error, got); continue; }
    const ok = canon(got) === canon(o.outcome);
    emit(o.id, ok ? "PASS" : "FAIL", ok ? `${got.outcome}` : "outcome mismatch", ok ? null : o.outcome, ok ? null : got);
  } catch (e) {
    const ok = o.status === "error" && e instanceof ConsecutiveError && e.message === o.error;
    emit(o.id, ok ? "PASS" : "FAIL", ok ? "err verbatim" : `${e.name}: ${e.message}`, o.error ?? "ok", e.message);
  }
}

// ---------- typed boundary (Node-only; unrepresentable in Rust) ----------
const bnd = (id, fn) => {
  try { fn(); emit(id, "FAIL", "accepted malformed input"); }
  catch (e) { emit(id, e instanceof ConsecutiveInputError ? "PASS" : "FAIL", `${e.name}: ${e.message.slice(0, 80)}`); }
};
const GD = () => [obs(2020,"100"),obs(2021,"90")];
bnd("reject_input_notarray", () => evaluate({}, R(2020,2021), params("down",1)));
bnd("reject_year_float", () => evaluate([{ ...obs(2020,"1"), year: 2020.5 }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_year_str", () => evaluate([{ ...obs(2020,"1"), year: "2020" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_date_junk", () => evaluate([{ ...obs(2020,"1"), first: "junk" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_date_feb30", () => evaluate([{ ...obs(2020,"1"), first: "2020-02-30" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_date_nopad", () => evaluate([{ ...obs(2020,"1"), first: "2020-1-2" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_close_bad", () => evaluate([{ ...obs(2020,"1"), close: "abc" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_close_num", () => evaluate([{ ...obs(2020,"1"), close: 1 }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_close_over96", () => evaluate([{ ...obs(2020,"1"), close: "79228162514264337593543950336" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_close_surrogate", () => evaluate([{ ...obs(2020,"1"), close: "1\ud800" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_completion_bad", () => evaluate([{ ...obs(2020,"1"), completion: "maybe" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_unfinished_str", () => evaluate([{ ...obs(2020,"1"), unfinished: "yes" }, obs(2021,"0.5")], R(2020,2021), params("down",1)));
bnd("reject_direction_bad", () => evaluate(GD(), R(2020,2021), params("sideways",1)));
bnd("reject_min_float", () => evaluate(GD(), R(2020,2021), params("down",1.5)));
bnd("reject_completed_only_str", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, completed_only:"yes" }));
bnd("reject_price_field", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, price_field:"open" }));
bnd("reject_equal_policy", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, equal_policy:"continue" }));
bnd("reject_range_str", () => evaluate(GD(), { start_year:"2020", end_year:2021 }, params("down",1)));
bnd("reject_obs_null", () => evaluate([null], R(2020,2020), params("down",1)));
// R1: Date.UTC-independent calendar + absent-only serde defaults
bnd("reject_date_1900_feb29", () => evaluate([{ ...obs(1900,"1"), first: "1900-02-29" }, obs(1901,"0.5")], R(1900,1901), params("down",1)));
bnd("reject_date_2100_feb29", () => evaluate([{ ...obs(2100,"1"), first: "2100-02-29" }, obs(2101,"0.5")], R(2100,2101), params("down",1)));
bnd("reject_completed_only_null", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, completed_only:null }));
bnd("reject_completed_only_zero", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, completed_only:0 }));
bnd("reject_price_field_null", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, price_field:null }));
bnd("reject_equal_policy_null", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, equal_policy:null }));
bnd("reject_gap_policy_null", () => evaluate(GD(), R(2020,2021), { direction:"down", min_periods:1, gap_policy:null }));

// selftest
if (SELFTEST === "consecutive") {
  const got = evaluate([obs(2020,"70"),obs(2021,"80"),obs(2022,"90"),obs(2023,"100")], R(2020,2023), params("up",3));
  emit("selftest_consecutive", got.run_length === 999 ? "PASS" : "FAIL",
    got.run_length === 999 ? null : `run_length=${got.run_length} != 999 — STOP`);
}

process.exitCode = failures === 0 ? 0 : 1;
