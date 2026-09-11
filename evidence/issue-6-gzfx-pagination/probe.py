#!/usr/bin/env python3
"""Reproduction template for the corrected bounded page checks.

It intentionally has six explicit calls, no retry and no pagination loop.
Running it would issue network requests; the committed raw responses were
captured separately and this template was not run during evidence packaging.
"""
import json
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import Request, urlopen

ENDPOINT = "https://datacenter-web.eastmoney.com/api/data/v1/get"
COMMON = {
    "reportName": "RPT_VALUEANALYSIS_DET",
    "columns": "ALL",
    "quoteColumns": "",
    "pageSize": 5,
    "sortColumns": "SECURITY_CODE",
    "sortTypes": 1,
    "source": "WEB",
    "client": "WEB",
}
FILTERS = {
    "shanghai": '(TRADE_DATE=\'2026-09-10\')(TRADE_MARKET in ("069001001001","069001001006","069001001003"))',
    "shenzhen": '(TRADE_DATE=\'2026-09-10\')(TRADE_MARKET in ("069001002001","069001002002","069001002005"))',
}
OUT = Path(__file__).parent


def fetch(name, market, page):
    params = {**COMMON, "pageNumber": page, "filter": FILTERS[market]}
    url = ENDPOINT + "?" + urlencode(params)
    with urlopen(Request(url, headers={"User-Agent": "x-data-mcp-evidence/1.0"}), timeout=15) as response:
        raw = response.read()
        (OUT / f"{name}.json").write_bytes(raw)
    return json.loads(raw)


def main():
    fetch("shanghai-page-2", "shanghai", 2)
    fetch("shanghai-page-464", "shanghai", 464)
    fetch("shanghai-page-465", "shanghai", 465)
    fetch("shenzhen-page-2", "shenzhen", 2)
    fetch("shenzhen-page-581", "shenzhen", 581)
    fetch("shenzhen-page-582", "shenzhen", 582)


if __name__ == "__main__":
    main()
