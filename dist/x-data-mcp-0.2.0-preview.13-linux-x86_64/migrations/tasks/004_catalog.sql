CREATE TABLE catalog_versions (
 version_id TEXT PRIMARY KEY, published_at TEXT NOT NULL, source TEXT NOT NULL,
 acquired_at TEXT NOT NULL, compat_json TEXT NOT NULL, digest TEXT NOT NULL
) STRICT;
CREATE TABLE catalog_members (
 version_id TEXT NOT NULL REFERENCES catalog_versions(version_id), exchange TEXT NOT NULL CHECK(exchange IN ('sh','sz')),
 code TEXT NOT NULL, name TEXT NOT NULL, source_security_id TEXT NOT NULL,
 industry_status TEXT NOT NULL CHECK(industry_status IN ('assigned','missing','conflicted')),
 industry_code TEXT, industry_name TEXT, industry_reason TEXT,
 PRIMARY KEY(version_id,exchange,code)
) STRICT;
CREATE TABLE catalog_head (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), version_id TEXT NOT NULL REFERENCES catalog_versions(version_id)
) STRICT;
