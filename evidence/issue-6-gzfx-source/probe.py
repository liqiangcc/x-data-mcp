#!/usr/bin/env python3
"""Reproduction template for the two bounded gzfx requests.

This file documents the already executed request shape. It is not run as part
of this evidence commit and has no retry or pagination loop.
"""
from urllib.parse import urlencode

ENDPOINT = "https://datacenter-web.eastmoney.com/api/data/v1/get"
BASE = {
    "reportName": "RPT_VALUEANALYSIS_DET",
    "columns": "ALL",
    "quoteColumns": "",
    "pageNumber": "1",
    "pageSize": "5",
    "sortColumns": "SECURITY_CODE",
    "sortTypes": "1",
    "source": "WEB",
    "client": "WEB",
}
FILTERS = {
    "shanghai": '(TRADE_DATE=\'2026-09-10\')(TRADE_MARKET in ("069001001001","069001001006","069001001003"))',
    "shenzhen": '(TRADE_DATE=\'2026-09-10\')(TRADE_MARKET in ("069001002001","069001002002","069001002005"))',
}

for market, filter_expression in FILTERS.items():
    params = {**BASE, "filter": filter_expression}
    print(market, ENDPOINT + "?" + urlencode(params))
