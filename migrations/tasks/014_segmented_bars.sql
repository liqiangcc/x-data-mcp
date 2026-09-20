CREATE TABLE bar_segments (
 segment_id TEXT PRIMARY KEY, symbol TEXT NOT NULL,
 first_date TEXT NOT NULL, last_date TEXT NOT NULL,
 row_count INTEGER NOT NULL CHECK(row_count>0)
) STRICT;
CREATE TABLE segment_bars (
 segment_id TEXT NOT NULL REFERENCES bar_segments(segment_id), trade_date TEXT NOT NULL,
 open TEXT NOT NULL, high TEXT NOT NULL, low TEXT NOT NULL, close TEXT NOT NULL,
 volume TEXT, amount TEXT, r7_raw TEXT,
 PRIMARY KEY(segment_id,trade_date)
) STRICT;
CREATE TABLE version_segments (
 version_id TEXT NOT NULL REFERENCES data_versions(version_id),
 seq INTEGER NOT NULL CHECK(seq>=0),
 segment_id TEXT NOT NULL REFERENCES bar_segments(segment_id),
 clip_start TEXT NOT NULL, clip_end TEXT NOT NULL,
 PRIMARY KEY(version_id,seq)
) STRICT;
INSERT INTO bar_segments SELECT 'seg-'||version_id, symbol, first_date, last_date, record_count
 FROM data_versions WHERE period='day';
INSERT INTO segment_bars SELECT 'seg-'||version_id, trade_date, open, high, low, close, volume, amount, r7_raw
 FROM daily_bars;
INSERT INTO version_segments SELECT version_id, 0, 'seg-'||version_id, first_date, last_date
 FROM data_versions WHERE period='day';
DROP TABLE daily_bars;
