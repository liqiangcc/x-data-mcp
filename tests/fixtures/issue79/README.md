# #79 independent legacy digest fixture

`legacy_record_digest.rs.txt` is the exact function from
`1e270c648e7884fffd65ef13cfe9f47e0dab22ad:src/storage.rs`.
The integration test executes this frozen implementation as an independent oracle;
production Stage B does not call this copy.

`legacy-records.json` uses ASCII keys/values and string Decimals. Expected SHA-256
values were calculated independently with Python hashlib over sorted compact JSON
(`json.dumps(sort_keys=True,separators=(',',':'),ensure_ascii=False)`) after removing
only top-level integrity_sha256. CI must also match the exact old Rust function;
this fixture is not a claim that Python JSON is generally serde_json-compatible.
Cases preserve absent versus null lineage, a hand-derived 2025 yearly lineage, precision beyond binary64, unknown
volume/amount and their units, coverage and explicit evidence. All data is fixture.
No real legacy database/source file is imported or re-certified.
