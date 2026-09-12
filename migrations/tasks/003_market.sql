CREATE TABLE data_versions (
 version_id TEXT PRIMARY KEY, symbol TEXT NOT NULL, period TEXT NOT NULL CHECK(period IN ('day','year')),
 adjustment TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('staging','validated','published')),
 source TEXT NOT NULL, published_at TEXT NOT NULL, record_count INTEGER NOT NULL, unique_count INTEGER NOT NULL, duplicate_count INTEGER NOT NULL,
 coverage_status TEXT NOT NULL, coverage_reason TEXT NOT NULL, first_date TEXT NOT NULL, last_date TEXT NOT NULL,
 compat_json TEXT NOT NULL, content_digest TEXT NOT NULL, digest_format TEXT NOT NULL
) STRICT;
CREATE TABLE version_evidence (
 version_id TEXT NOT NULL REFERENCES data_versions(version_id), field TEXT NOT NULL, status TEXT NOT NULL, value TEXT, reason TEXT NOT NULL,
 PRIMARY KEY(version_id,field)
) STRICT;
CREATE TABLE daily_bars (
 version_id TEXT NOT NULL REFERENCES data_versions(version_id), trade_date TEXT NOT NULL,
 open TEXT NOT NULL, high TEXT NOT NULL, low TEXT NOT NULL, close TEXT NOT NULL, volume TEXT, amount TEXT,
 PRIMARY KEY(version_id,trade_date)
) STRICT;
CREATE TABLE yearly_bars (
 version_id TEXT NOT NULL REFERENCES data_versions(version_id), year INTEGER NOT NULL, trade_date TEXT NOT NULL,
 open TEXT NOT NULL, high TEXT NOT NULL, low TEXT NOT NULL, close TEXT NOT NULL, volume TEXT, amount TEXT,
 first_trade_date TEXT NOT NULL, last_trade_date TEXT NOT NULL, is_finished INTEGER NOT NULL CHECK(is_finished IN (0,1)),
 PRIMARY KEY(version_id,year)
) STRICT;
CREATE TABLE yearly_lineage (
 version_id TEXT PRIMARY KEY REFERENCES data_versions(version_id), base_daily_version_id TEXT NOT NULL REFERENCES data_versions(version_id),
 algorithm_version TEXT NOT NULL, first_trade_date TEXT NOT NULL, last_trade_date TEXT NOT NULL, as_of TEXT NOT NULL
) STRICT;
CREATE TABLE publication_batches (
 publication_id TEXT PRIMARY KEY, version_id TEXT NOT NULL UNIQUE REFERENCES data_versions(version_id),
 task_id TEXT NOT NULL REFERENCES tasks(task_id), item_id TEXT NOT NULL, command_json TEXT NOT NULL, receipt_json TEXT NOT NULL,
 FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id)
) STRICT;
CREATE TABLE task_publications (
 task_id TEXT NOT NULL REFERENCES tasks(task_id), version_id TEXT NOT NULL UNIQUE REFERENCES data_versions(version_id),
 publication_id TEXT NOT NULL UNIQUE REFERENCES publication_batches(publication_id), PRIMARY KEY(task_id,version_id)
) STRICT;
CREATE TABLE effective_heads (
 symbol TEXT NOT NULL, period TEXT NOT NULL CHECK(period IN ('day','year')), adjustment TEXT NOT NULL,
 version_id TEXT NOT NULL UNIQUE REFERENCES data_versions(version_id), PRIMARY KEY(symbol,period,adjustment)
) STRICT;
CREATE INDEX market_versions_scope ON data_versions(symbol,period,adjustment,state,published_at,version_id);
