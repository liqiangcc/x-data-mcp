import ast
import json
import subprocess
from pathlib import Path
from urllib.parse import urlencode


def git_show(path):
    return subprocess.check_output(
        ["git", "-C", "/root/x-data-mcp", "show", "origin/main:" + path],
        text=True,
    )


evidence = json.loads(git_show("evidence/issue-6-gzfx-source/probe-result.json"))
probe_text = git_show("evidence/issue-6-gzfx-source/probe.py")
tree = ast.parse(probe_text)
values = {}
for node in tree.body:
    if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
        try:
            values[node.targets[0].id] = ast.literal_eval(node.value)
        except (ValueError, SyntaxError):
            pass

expected_filters = {
    "shanghai": evidence["responses"][0]["filter"],
    "shenzhen": evidence["responses"][1]["filter"],
}
probe_filters = values["FILTERS"]
common = {"reportName": evidence["reportName"], **evidence["common_params"]}
probe_common = {"reportName": "RPT_VALUEANALYSIS_DET", **values["BASE"]}
probe_common = {k: str(v) for k, v in probe_common.items()}
semantic_common = {k: str(v) for k, v in common.items()}
common_diff = {
    k: {"evidence": semantic_common.get(k), "probe": probe_common.get(k)}
    for k in sorted(set(semantic_common) | set(probe_common))
    if semantic_common.get(k) != probe_common.get(k)
}
filter_diff = {
    market: {"evidence": expected_filters[market], "probe": probe_filters[market]}
    for market in expected_filters
    if expected_filters[market] != probe_filters[market]
}
page1_urls = {
    market: "https://datacenter-web.eastmoney.com/api/data/v1/get?" + urlencode(
        {**common, "filter": filter_value}
    )
    for market, filter_value in expected_filters.items()
}
checks = {
    "common_params_field_diff": common_diff,
    "filter_field_diff": filter_diff,
    "date_single_quotes": all("(TRADE_DATE='2026-09-10')" in f for f in expected_filters.values()),
    "market_double_quotes_complete": all(
        f.count('"') == 6 and "TRADE_MARKET in" in f for f in expected_filters.values()
    ),
    "page_size_is_5": common["pageSize"] == 5,
    "page_number_is_1_for_comparison": common["pageNumber"] == 1,
}
result = {
    "network_requests": 0,
    "source": "origin/main evidence/issue-6-gzfx-source",
    "filters_repr": {k: repr(v) for k, v in expected_filters.items()},
    "page1_urls": page1_urls,
    "checks": checks,
    "preflight_pass": not common_diff and not filter_diff and all(
        value is True for key, value in checks.items() if key not in {"common_params_field_diff", "filter_field_diff"}
    ),
}
print(json.dumps(result, ensure_ascii=False, indent=2))
Path("/tmp/gzfx-corrected-preflight.json").write_text(
    json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
)
if not result["preflight_pass"]:
    raise SystemExit(1)
