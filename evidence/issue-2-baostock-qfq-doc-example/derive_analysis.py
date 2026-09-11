import json
import sys
from decimal import Decimal, ROUND_HALF_UP, localcontext
from pathlib import Path


source_path, output_path = map(Path, sys.argv[1:])
source = json.loads(source_path.read_text(encoding="utf-8"))
qfq_rows = {row[0]: row for row in source["qfq"]["response"]["rows"]}
none_rows = {row[0]: row for row in source["none"]["response"]["rows"]}
fields = {name: i for i, name in enumerate(source["qfq"]["response"]["fields"])}
compare_fields = ["open", "close", "preclose"]
expected_ratios = {
    "2017-05-24": Decimal("0.533312"),
    "2017-05-25": Decimal("0.702156"),
    "2017-05-26": Decimal("0.702156"),
}
old_factor = Decimal("0.759535")
later_scale = Decimal("0.702156")
old_factor_product = old_factor * later_scale
observed_first_ratio = expected_ratios["2017-05-24"]
display_quantum = Decimal("0.000001")

comparisons = []
for date in sorted(qfq_rows):
    for field in compare_fields:
        qfq = Decimal(qfq_rows[date][fields[field]])
        no_adjust = Decimal(none_rows[date][fields[field]])
        expected = expected_ratios[date]
        with localcontext() as context:
            context.prec = 80
            ratio = qfq / no_adjust
        comparisons.append(
            {
                "date": date,
                "field": field,
                "live_qfq": qfq_rows[date][fields[field]],
                "live_no_adjust": none_rows[date][fields[field]],
                "ratio_decimal": str(ratio),
                "expected_ratio": str(expected),
                "exact_decimal_equal": qfq == no_adjust * expected,
                "cross_multiplication_residual": str(qfq - no_adjust * expected),
            }
        )

for item in comparisons:
    assert item["exact_decimal_equal"]
    assert Decimal(item["cross_multiplication_residual"]) == 0

output = {
    "schema": "x-data-mcp.issue-2.baostock-qfq-doc-example.derived-analysis.v1",
    "source_probe_result": "probe-result.json",
    "method": "Python Decimal only; no float and no production tolerance",
    "comparison_fields": compare_fields,
    "ratios": comparisons,
    "uniform_scale_checks": {
        "2017-05-24": {
            "expected_ratio": "0.533312",
            "all_three_fields_exact_decimal_equal": all(
                x["date"] != "2017-05-24" or x["exact_decimal_equal"] for x in comparisons
            ),
        },
        "2017-05-25": {
            "expected_ratio": "0.702156",
            "all_three_fields_exact_decimal_equal": all(
                x["date"] != "2017-05-25" or x["exact_decimal_equal"] for x in comparisons
            ),
        },
        "2017-05-26": {
            "expected_ratio": "0.702156",
            "all_three_fields_exact_decimal_equal": all(
                x["date"] != "2017-05-26" or x["exact_decimal_equal"] for x in comparisons
            ),
        },
    },
    "official_factor_relationship": {
        "old_factor_2016_06_23": "0.759535",
        "later_scale_observed_2017_05_25_26": "0.702156",
        "product_decimal": str(old_factor_product),
        "observed_2017_05_24_ratio": str(observed_first_ratio),
        "exact_decimal_equal": old_factor_product == observed_first_ratio,
        "difference_observed_minus_product": str(observed_first_ratio - old_factor_product),
        "product_rounded_to_documented_six_places": str(old_factor_product.quantize(display_quantum, rounding=ROUND_HALF_UP)),
        "classification": "compatible at displayed factor precision; not exact equality",
    },
    "interpretation_boundary": {
        "later_scale_identity_verified": False,
        "all_subsequent_corporate_actions_verified": False,
        "historical_revision_semantics_verified": False,
        "production_authorization_verified": False,
    },
}
output_path.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
