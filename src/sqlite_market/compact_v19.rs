use super::*;
use crate::compact_market::{self, CompactBar, DeltaKind, DeltaOp, Digest, SegmentClip};
use crate::progress_trace::{Span, Stage};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredSegment {
    pub key: i64,
    pub digest: Digest,
    pub entity: String,
    pub period: Period,
    pub bars: Vec<CompactBar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredClip {
    pub segment_key: i64,
    pub clip: SegmentClip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoredDeltaKind {
    Upsert(StoredClip),
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredDelta {
    pub range_start_day: i64,
    pub range_end_day: i64,
    pub kind: StoredDeltaKind,
}

pub(crate) fn insert_segment(
    c: &Connection,
    entity: &str,
    period: Period,
    bars: &[CompactBar],
) -> Result<StoredSegment, Error> {
    let digest = compact_market::segment_digest(entity, period, bars).map_err(integrity)?;
    let existing: Option<i64> = c
        .query_row(
            "SELECT segment_key FROM bar_segments WHERE segment_digest=?1",
            params![digest.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(key) = existing {
        let stored = read_segment(c, key)?;
        if stored.entity != entity || stored.period != period || stored.bars != bars {
            return Err(integrity("segment digest collision with different content"));
        }
        return Ok(stored);
    }
    c.execute(
        "INSERT INTO bar_segments(segment_digest,entity,period,first_day,last_day,row_count) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            digest.as_slice(),
            entity,
            period_code(period),
            bars.first().unwrap().trade_day,
            bars.last().unwrap().trade_day,
            bars.len() as i64
        ],
    )
    .map_err(storage)?;
    let key = c.last_insert_rowid();
    let mut statement = c
        .prepare(
            "INSERT INTO segment_bars(segment_key,trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .map_err(storage)?;
    for bar in bars {
        statement
            .execute(params![
                key,
                bar.trade_day,
                bar.open_cents,
                bar.high_cents,
                bar.low_cents,
                bar.close_cents,
                bar.volume,
                bar.amount,
                bar.r7_raw.map(i64::from)
            ])
            .map_err(storage)?;
    }
    Ok(StoredSegment {
        key,
        digest,
        entity: entity.to_string(),
        period,
        bars: bars.to_vec(),
    })
}

pub(crate) fn read_segment(c: &Connection, key: i64) -> Result<StoredSegment, Error> {
    #[cfg(test)]
    super::compact_versions::read_counts::count_segment_read();
    let sql_span = Span::enter(Stage::SegmentSql);
    let mut header = c
        .prepare_cached(
            "SELECT segment_digest,entity,period,first_day,last_day,row_count FROM bar_segments WHERE segment_key=?1",
        )
        .map_err(storage)?;
    let row = header
        .query_row(params![key], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .optional()
        .map_err(storage)?;
    drop(header);
    let (digest_bytes, entity, period_code, first_day, last_day, row_count) =
        row.ok_or_else(|| integrity("segment missing"))?;
    let digest: Digest = digest_bytes
        .try_into()
        .map_err(|_| integrity("segment digest length mismatch"))?;
    let period = decode_period(period_code)?;
    let mut statement = c
        .prepare_cached(
            "SELECT trade_day,open_cents,high_cents,low_cents,close_cents,volume,amount,r7_raw FROM segment_bars WHERE segment_key=?1 ORDER BY trade_day",
        )
        .map_err(storage)?;
    // Rows stream straight into the bar vector — no tuple staging; the
    // declared row_count bounds only the reserve hint (never the check).
    let mut bars =
        Vec::with_capacity(row_count.clamp(0, compact_market::MAX_SEGMENT_BARS as i64) as usize);
    {
        let mut rows = statement.query(params![key]).map_err(storage)?;
        while let Some(r) = rows.next().map_err(storage)? {
            bars.push(CompactBar {
                trade_day: r.get(0).map_err(storage)?,
                open_cents: r.get(1).map_err(storage)?,
                high_cents: r.get(2).map_err(storage)?,
                low_cents: r.get(3).map_err(storage)?,
                close_cents: r.get(4).map_err(storage)?,
                volume: r.get(5).map_err(storage)?,
                amount: r.get(6).map_err(storage)?,
                r7_raw: r
                    .get::<_, Option<i64>>(7)
                    .map_err(storage)?
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| integrity("r7_raw out of u32 range"))?,
            });
        }
    }
    drop(statement);
    drop(sql_span);
    crate::progress_trace::segment_rows(bars.len());
    if bars.len() as i64 != row_count {
        return Err(integrity("segment row_count mismatch"));
    }
    if let (Some(first), Some(last)) = (bars.first(), bars.last())
        && (first.trade_day != first_day || last.trade_day != last_day)
    {
        return Err(integrity("segment day bounds mismatch"));
    }
    // No preliminary per-bar decode: `segment_digest` below runs every
    // bar through the same checked decoder before its digest bytes feed
    // the comparison — malformed bars fail closed there.
    let digest_span = Span::enter(Stage::SegmentDigest);
    let recomputed = compact_market::segment_digest(&entity, period, &bars).map_err(integrity)?;
    drop(digest_span);
    if recomputed != digest {
        return Err(integrity("segment digest mismatch"));
    }
    Ok(StoredSegment {
        key,
        digest,
        entity,
        period,
        bars,
    })
}

/// Retained-segment bounds for a [`SegmentReadCtx`]: at most 128 verified
/// segments AND 8 MiB of accounted payload, whichever binds first.
const SEGMENT_CTX_CAP: usize = 128;
const SEGMENT_CTX_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Read-transaction-scoped verified segment cache (GOAL-008 XP-011).
///
/// A `SegmentReadCtx` is bound to a single `Connection`/read transaction
/// and dropped with it — it is never shared across requests, databases,
/// commits or writes. A miss runs the unchanged full `read_segment`
/// (row/bounds/decode/entity-period/digest checks); only a segment that
/// passed every check is retained, and a hit reuses that verified
/// `Rc<StoredSegment>` inside the same SQLite snapshot. Clip-level
/// entity/period/range/count/projection checks still run at every use.
///
/// `reads`/`hits` count actual full reads and cache hits so tests assert
/// measured counts instead of inferring reuse from `Rc::ptr_eq`.
#[derive(Default)]
pub(crate) struct SegmentReadCtx {
    entries: HashMap<i64, Rc<StoredSegment>>,
    lru: VecDeque<i64>,
    retained_bytes: usize,
    owner: usize,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    hits: usize,
}

impl SegmentReadCtx {
    /// (full `read_segment` executions, cache hits, retained entries,
    /// retained accounted bytes).
    #[cfg(test)]
    pub(crate) fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.reads,
            self.hits,
            self.entries.len(),
            self.retained_bytes,
        )
    }

    /// The cache is scoped to one `Connection`; a second connection means
    /// the caller is reusing the context across databases or snapshots.
    fn check_owner(&mut self, c: &Connection) -> Result<(), Error> {
        let id = c as *const Connection as usize;
        if self.owner == 0 {
            self.owner = id;
        } else if self.owner != id {
            return Err(integrity("segment read context reused across connections"));
        }
        Ok(())
    }

    /// Retain a fully verified segment under the LRU/byte bounds. An
    /// oversized or overflowing entry is silently skipped — capacity is
    /// never a reason to fail a valid read.
    fn retain(&mut self, segment: &Rc<StoredSegment>) {
        let Some(bytes) = segment_footprint(segment) else {
            return;
        };
        if bytes > SEGMENT_CTX_MAX_BYTES {
            return;
        }
        while let Some(&evicted) = self.lru.front() {
            if self.entries.len() < SEGMENT_CTX_CAP
                && self.retained_bytes.saturating_add(bytes) <= SEGMENT_CTX_MAX_BYTES
            {
                break;
            }
            self.lru.pop_front();
            if let Some(old) = self.entries.remove(&evicted)
                && let Some(old_bytes) = segment_footprint(&old)
            {
                self.retained_bytes = self.retained_bytes.saturating_sub(old_bytes);
            }
        }
        if self.entries.len() >= SEGMENT_CTX_CAP
            || self.retained_bytes.saturating_add(bytes) > SEGMENT_CTX_MAX_BYTES
        {
            return;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.entries.insert(segment.key, segment.clone());
        self.lru.push_back(segment.key);
    }
}

/// Accounted retained payload of one cached segment: the `StoredSegment`
/// struct, the entity string capacity, the bar-vector capacity and each
/// amount string capacity. `None` on arithmetic overflow — the caller
/// treats the entry as oversized and never retains it.
fn segment_footprint(segment: &StoredSegment) -> Option<usize> {
    let mut total = std::mem::size_of::<StoredSegment>()
        .checked_add(segment.entity.capacity())?
        .checked_add(
            segment
                .bars
                .capacity()
                .checked_mul(std::mem::size_of::<CompactBar>())?,
        )?;
    for bar in &segment.bars {
        if let Some(amount) = &bar.amount {
            total = total.checked_add(amount.capacity())?;
        }
    }
    Some(total)
}

/// `read_segment` through a transaction-scoped verified cache. A miss
/// runs the unchanged full read and retains the result only after every
/// check passed; a hit reuses the already-verified segment of this
/// snapshot. Capacity never fails a read: oversized or evicted entries
/// are returned verified but unretained.
pub(crate) fn read_segment_ctx(
    c: &Connection,
    key: i64,
    ctx: &mut SegmentReadCtx,
) -> Result<Rc<StoredSegment>, Error> {
    ctx.check_owner(c)?;
    if let Some(hit) = ctx.entries.get(&key) {
        let segment = hit.clone();
        #[cfg(test)]
        {
            ctx.hits += 1;
        }
        ctx.lru.retain(|k| *k != key);
        ctx.lru.push_back(key);
        return Ok(segment);
    }
    let segment = Rc::new(read_segment(c, key)?);
    #[cfg(test)]
    {
        ctx.reads += 1;
    }
    ctx.retain(&segment);
    Ok(segment)
}

/// `read_segment` under an optional segment context — the single read
/// seam shared by the cached (`Some`) and uncached (`None`, write-path
/// and reference-test) read variants.
pub(crate) fn read_segment_scoped(
    c: &Connection,
    key: i64,
    sctx: Option<&mut SegmentReadCtx>,
) -> Result<Rc<StoredSegment>, Error> {
    match sctx {
        Some(ctx) => read_segment_ctx(c, key, ctx),
        None => Ok(Rc::new(read_segment(c, key)?)),
    }
}

pub(crate) fn write_checkpoint(
    c: &Connection,
    version_key: i64,
    entity: &str,
    period: Period,
    clips: &[StoredClip],
) -> Result<Digest, Error> {
    let resolved = resolve_clips(c, entity, period, clips)?;
    let digest = compact_market::checkpoint_projection_digest(entity, period, &resolved)
        .map_err(integrity)?;
    reject_existing_version_rows(c, "version_checkpoint_clips", version_key)?;
    let mut statement = c
        .prepare(
            "INSERT INTO version_checkpoint_clips(version_key,seq,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?1,?2,?3,?4,?5,?6)",
        )
        .map_err(storage)?;
    for (seq, stored) in clips.iter().enumerate() {
        statement
            .execute(params![
                version_key,
                seq as i64,
                stored.segment_key,
                stored.clip.clip_start_day,
                stored.clip.clip_end_day,
                stored.clip.row_count
            ])
            .map_err(storage)?;
    }
    Ok(digest)
}

pub(crate) fn read_checkpoint(
    c: &Connection,
    version_key: i64,
    entity: &str,
    period: Period,
) -> Result<(Vec<StoredClip>, Digest), Error> {
    read_checkpoint_scoped(c, version_key, entity, period, None)
}

/// `read_checkpoint` reusing verified segments through the caller's
/// transaction-scoped context. Clip checks are unchanged — they run on
/// every call against the (cached or freshly read) segment.
pub(crate) fn read_checkpoint_ctx(
    c: &Connection,
    version_key: i64,
    entity: &str,
    period: Period,
    sctx: &mut SegmentReadCtx,
) -> Result<(Vec<StoredClip>, Digest), Error> {
    read_checkpoint_scoped(c, version_key, entity, period, Some(sctx))
}

fn read_checkpoint_scoped(
    c: &Connection,
    version_key: i64,
    entity: &str,
    period: Period,
    mut sctx: Option<&mut SegmentReadCtx>,
) -> Result<(Vec<StoredClip>, Digest), Error> {
    let mut statement = c
        .prepare(
            "SELECT seq,segment_key,clip_start_day,clip_end_day,row_count FROM version_checkpoint_clips WHERE version_key=?1 ORDER BY seq",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![version_key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(statement);
    if rows.is_empty() {
        return Err(integrity("checkpoint missing"));
    }
    let mut clips = Vec::with_capacity(rows.len());
    for (index, (seq, segment_key, clip_start_day, clip_end_day, row_count)) in
        rows.iter().enumerate()
    {
        if *seq != index as i64 {
            return Err(integrity("checkpoint seq not contiguous"));
        }
        if *row_count <= 0 || *row_count > u32::MAX as i64 {
            return Err(integrity("checkpoint clip row_count out of bounds"));
        }
        let segment = read_segment_scoped(c, *segment_key, sctx.as_deref_mut())?;
        let clip = SegmentClip {
            segment_digest: segment.digest,
            clip_start_day: *clip_start_day,
            clip_end_day: *clip_end_day,
            row_count: *row_count as u32,
        };
        check_clip_against_segment(&segment, &clip, entity, period)?;
        clips.push(StoredClip {
            segment_key: *segment_key,
            clip,
        });
    }
    let plain: Vec<SegmentClip> = clips.iter().map(|stored| stored.clip.clone()).collect();
    let digest =
        compact_market::checkpoint_projection_digest(entity, period, &plain).map_err(integrity)?;
    Ok((clips, digest))
}

pub(crate) fn write_delta(
    c: &Connection,
    version_key: i64,
    parent_projection: Digest,
    ops: &[StoredDelta],
) -> Result<(Digest, Digest), Error> {
    let mut plain = Vec::with_capacity(ops.len());
    for op in ops {
        match &op.kind {
            StoredDeltaKind::Upsert(stored) => {
                let segment = read_segment(c, stored.segment_key)?;
                check_clip_against_segment(
                    &segment,
                    &stored.clip,
                    &segment.entity,
                    segment.period,
                )?;
                plain.push(DeltaOp {
                    range_start_day: op.range_start_day,
                    range_end_day: op.range_end_day,
                    kind: DeltaKind::Upsert(stored.clip.clone()),
                });
            }
            StoredDeltaKind::Remove => plain.push(DeltaOp {
                range_start_day: op.range_start_day,
                range_end_day: op.range_end_day,
                kind: DeltaKind::Remove,
            }),
        }
    }
    let (delta, projection) =
        compact_market::delta_projection_digest(parent_projection, &plain).map_err(integrity)?;
    reject_existing_version_rows(c, "version_delta_clips", version_key)?;
    let mut statement = c
        .prepare(
            "INSERT INTO version_delta_clips(version_key,seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .map_err(storage)?;
    for (seq, op) in ops.iter().enumerate() {
        let (kind, segment_key, clip_start, clip_end, row_count) = match &op.kind {
            StoredDeltaKind::Upsert(stored) => (
                1i64,
                Some(stored.segment_key),
                Some(stored.clip.clip_start_day),
                Some(stored.clip.clip_end_day),
                i64::from(stored.clip.row_count),
            ),
            StoredDeltaKind::Remove => (2i64, None, None, None, 0i64),
        };
        statement
            .execute(params![
                version_key,
                seq as i64,
                kind,
                op.range_start_day,
                op.range_end_day,
                segment_key,
                clip_start,
                clip_end,
                row_count
            ])
            .map_err(storage)?;
    }
    Ok((delta, projection))
}

pub(crate) fn read_delta(
    c: &Connection,
    version_key: i64,
    parent_projection: Digest,
) -> Result<(Vec<StoredDelta>, Digest, Digest), Error> {
    read_delta_scoped(c, version_key, parent_projection, None)
}

/// `read_delta` reusing verified segments through the caller's
/// transaction-scoped context. Clip checks are unchanged — they run on
/// every call against the (cached or freshly read) segment.
pub(crate) fn read_delta_ctx(
    c: &Connection,
    version_key: i64,
    parent_projection: Digest,
    sctx: &mut SegmentReadCtx,
) -> Result<(Vec<StoredDelta>, Digest, Digest), Error> {
    read_delta_scoped(c, version_key, parent_projection, Some(sctx))
}

fn read_delta_scoped(
    c: &Connection,
    version_key: i64,
    parent_projection: Digest,
    mut sctx: Option<&mut SegmentReadCtx>,
) -> Result<(Vec<StoredDelta>, Digest, Digest), Error> {
    let mut statement = c
        .prepare(
            "SELECT seq,delta_kind,range_start_day,range_end_day,segment_key,clip_start_day,clip_end_day,row_count FROM version_delta_clips WHERE version_key=?1 ORDER BY seq",
        )
        .map_err(storage)?;
    let rows = statement
        .query_map(params![version_key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(statement);
    if rows.is_empty() {
        return Err(integrity("delta missing"));
    }
    let mut ops = Vec::with_capacity(rows.len());
    let mut plain = Vec::with_capacity(rows.len());
    for (
        index,
        (seq, kind, range_start, range_end, segment_key, clip_start, clip_end, row_count),
    ) in rows.iter().enumerate()
    {
        if *seq != index as i64 {
            return Err(integrity("delta seq not contiguous"));
        }
        let (stored_kind, plain_kind) = match kind {
            1 => {
                let (segment_key, clip_start, clip_end) = match (segment_key, clip_start, clip_end)
                {
                    (Some(key), Some(start), Some(end)) => (*key, *start, *end),
                    _ => return Err(integrity("upsert delta missing clip fields")),
                };
                if *row_count <= 0 || *row_count > u32::MAX as i64 {
                    return Err(integrity("upsert delta row_count out of bounds"));
                }
                let segment = read_segment_scoped(c, segment_key, sctx.as_deref_mut())?;
                let clip = SegmentClip {
                    segment_digest: segment.digest,
                    clip_start_day: clip_start,
                    clip_end_day: clip_end,
                    row_count: *row_count as u32,
                };
                check_clip_against_segment(&segment, &clip, &segment.entity, segment.period)?;
                (
                    StoredDeltaKind::Upsert(StoredClip {
                        segment_key,
                        clip: clip.clone(),
                    }),
                    DeltaKind::Upsert(clip),
                )
            }
            2 => {
                if segment_key.is_some()
                    || clip_start.is_some()
                    || clip_end.is_some()
                    || *row_count != 0
                {
                    return Err(integrity("remove delta carries clip fields"));
                }
                (StoredDeltaKind::Remove, DeltaKind::Remove)
            }
            _ => return Err(integrity("unknown delta_kind")),
        };
        ops.push(StoredDelta {
            range_start_day: *range_start,
            range_end_day: *range_end,
            kind: stored_kind,
        });
        plain.push(DeltaOp {
            range_start_day: *range_start,
            range_end_day: *range_end,
            kind: plain_kind,
        });
    }
    let (delta, projection) =
        compact_market::delta_projection_digest(parent_projection, &plain).map_err(integrity)?;
    Ok((ops, delta, projection))
}

fn resolve_clips(
    c: &Connection,
    entity: &str,
    period: Period,
    clips: &[StoredClip],
) -> Result<Vec<SegmentClip>, Error> {
    let mut resolved = Vec::with_capacity(clips.len());
    for stored in clips {
        let segment = read_segment(c, stored.segment_key)?;
        if segment.digest != stored.clip.segment_digest {
            return Err(integrity("clip segment digest mismatch"));
        }
        check_clip_against_segment(&segment, &stored.clip, entity, period)?;
        resolved.push(stored.clip.clone());
    }
    Ok(resolved)
}

fn check_clip_against_segment(
    segment: &StoredSegment,
    clip: &SegmentClip,
    entity: &str,
    period: Period,
) -> Result<(), Error> {
    if segment.entity != entity || segment.period != period {
        return Err(integrity("clip segment entity/period mismatch"));
    }
    if clip.segment_digest != segment.digest {
        return Err(integrity("clip segment digest mismatch"));
    }
    if clip.clip_start_day < segment.bars.first().unwrap().trade_day
        || clip.clip_end_day > segment.bars.last().unwrap().trade_day
    {
        return Err(integrity("clip bounds outside segment"));
    }
    let actual = segment
        .bars
        .iter()
        .filter(|bar| bar.trade_day >= clip.clip_start_day && bar.trade_day <= clip.clip_end_day)
        .count();
    if actual as u32 != clip.row_count {
        return Err(integrity("clip row_count does not match segment rows"));
    }
    Ok(())
}

fn reject_existing_version_rows(
    c: &Connection,
    table: &str,
    version_key: i64,
) -> Result<(), Error> {
    let count: i64 = c
        .query_row(
            &format!("SELECT count(*) FROM {table} WHERE version_key=?1"),
            params![version_key],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if count != 0 {
        return Err(integrity("version clip rows already exist"));
    }
    Ok(())
}

fn period_code(period: Period) -> i64 {
    match period {
        Period::Day => 1,
        Period::Year => 2,
    }
}

fn decode_period(code: i64) -> Result<Period, Error> {
    match code {
        1 => Ok(Period::Day),
        2 => Ok(Period::Year),
        _ => Err(integrity("unknown period code")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE data_versions(version_key INTEGER PRIMARY KEY) STRICT;
             CREATE TABLE bar_segments(
               segment_key INTEGER PRIMARY KEY,
               segment_digest BLOB NOT NULL UNIQUE CHECK(length(segment_digest)=32),
               entity TEXT NOT NULL,
               period INTEGER NOT NULL CHECK(period IN (1,2)),
               first_day INTEGER NOT NULL,
               last_day INTEGER NOT NULL,
               row_count INTEGER NOT NULL CHECK(row_count BETWEEN 1 AND 512),
               CHECK(first_day <= last_day)
             ) STRICT;
             CREATE TABLE segment_bars(
               segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
               trade_day INTEGER NOT NULL,
               open_cents INTEGER NOT NULL,
               high_cents INTEGER NOT NULL,
               low_cents INTEGER NOT NULL,
               close_cents INTEGER NOT NULL,
               volume INTEGER,
               amount TEXT,
               r7_raw INTEGER,
               PRIMARY KEY(segment_key,trade_day),
               CHECK(high_cents >= open_cents AND high_cents >= close_cents AND high_cents >= low_cents),
               CHECK(low_cents <= open_cents AND low_cents <= close_cents),
               CHECK(volume IS NULL OR volume >= 0),
               CHECK(r7_raw IS NULL OR r7_raw BETWEEN 0 AND 4294967295)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE version_checkpoint_clips(
               version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
               seq INTEGER NOT NULL CHECK(seq >= 0),
               segment_key INTEGER NOT NULL REFERENCES bar_segments(segment_key),
               clip_start_day INTEGER NOT NULL,
               clip_end_day INTEGER NOT NULL,
               row_count INTEGER NOT NULL CHECK(row_count > 0),
               PRIMARY KEY(version_key,seq),
               CHECK(clip_start_day <= clip_end_day)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE version_delta_clips(
               version_key INTEGER NOT NULL REFERENCES data_versions(version_key),
               seq INTEGER NOT NULL CHECK(seq >= 0),
               delta_kind INTEGER NOT NULL CHECK(delta_kind IN (1,2)),
               range_start_day INTEGER NOT NULL,
               range_end_day INTEGER NOT NULL,
               segment_key INTEGER REFERENCES bar_segments(segment_key),
               clip_start_day INTEGER,
               clip_end_day INTEGER,
               row_count INTEGER NOT NULL CHECK(row_count >= 0),
               PRIMARY KEY(version_key,seq),
               CHECK(range_start_day <= range_end_day),
               CHECK((delta_kind=1 AND segment_key IS NOT NULL AND clip_start_day IS NOT NULL
                      AND clip_end_day IS NOT NULL AND row_count > 0)
                  OR (delta_kind=2 AND segment_key IS NULL AND clip_start_day IS NULL
                      AND clip_end_day IS NULL AND row_count=0))
             ) STRICT, WITHOUT ROWID;",
        )
        .unwrap();
        c
    }

    fn bars() -> Vec<CompactBar> {
        vec![
            CompactBar {
                trade_day: 0,
                open_cents: -123,
                high_cents: -100,
                low_cents: -200,
                close_cents: -150,
                volume: Some(10),
                amount: Some("12.34".into()),
                r7_raw: Some(7),
            },
            CompactBar {
                trade_day: 1,
                open_cents: 100,
                high_cents: 200,
                low_cents: 50,
                close_cents: 150,
                volume: None,
                amount: None,
                r7_raw: None,
            },
            CompactBar {
                trade_day: 5,
                open_cents: 100,
                high_cents: 200,
                low_cents: 50,
                close_cents: 150,
                volume: Some(3),
                amount: None,
                r7_raw: None,
            },
        ]
    }

    fn version(c: &Connection, key: i64) {
        c.execute(
            "INSERT INTO data_versions(version_key) VALUES(?1)",
            params![key],
        )
        .unwrap();
    }

    fn stored_clip(segment: &StoredSegment, start: i64, end: i64, count: u32) -> StoredClip {
        StoredClip {
            segment_key: segment.key,
            clip: SegmentClip {
                segment_digest: segment.digest,
                clip_start_day: start,
                clip_end_day: end,
                row_count: count,
            },
        }
    }

    fn integrity_failure<T>(result: Result<T, Error>) {
        match result {
            Err(Error::Integrity(_)) => {}
            Err(e) => panic!("expected Integrity failure, got {e}"),
            Ok(_) => panic!("expected Integrity failure, got Ok"),
        }
    }

    #[test]
    fn insert_read_and_reuse_segment() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let read = read_segment(&c, stored.key).unwrap();
        assert_eq!(read.digest, stored.digest);
        assert_eq!(read.bars, stored.bars);
        assert_eq!(read.entity, "sh.600006");
        assert_eq!(read.period, Period::Day);
        let reused = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        assert_eq!(reused.key, stored.key);
        let count: i64 = c
            .query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn negative_price_row_roundtrips() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let read = read_segment(&c, stored.key).unwrap();
        assert_eq!(read.bars[0].open_cents, -123);
        assert_eq!(
            read.bars[0].to_bar(Period::Day).unwrap().open,
            Decimal::new(-123, 2)
        );
    }

    #[test]
    fn tampered_bar_fails_integrity() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        c.execute(
            "UPDATE segment_bars SET high_cents=high_cents+100 WHERE segment_key=?1 AND trade_day=1",
            params![stored.key],
        )
        .unwrap();
        integrity_failure(read_segment(&c, stored.key));
    }

    #[test]
    fn deleted_bar_row_fails_integrity() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        c.execute(
            "DELETE FROM segment_bars WHERE segment_key=?1 AND trade_day=5",
            params![stored.key],
        )
        .unwrap();
        integrity_failure(read_segment(&c, stored.key));
    }

    #[test]
    fn altered_segment_digest_fails_integrity() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        c.execute(
            "UPDATE bar_segments SET segment_digest=?1 WHERE segment_key=?2",
            params![[0xabu8; 32].as_slice(), stored.key],
        )
        .unwrap();
        integrity_failure(read_segment(&c, stored.key));
        let missing = read_segment(&c, stored.key + 99);
        integrity_failure(missing);
    }

    #[test]
    fn checkpoint_write_read_roundtrip() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let clips = vec![
            stored_clip(&segment, 0, 1, 2),
            stored_clip(&segment, 5, 5, 1),
        ];
        let digest = write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).unwrap();
        let (read, read_digest) = read_checkpoint(&c, 1, "sh.600006", Period::Day).unwrap();
        assert_eq!(read_digest, digest);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].clip, clips[0].clip);
        integrity_failure(write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).map(|_| ()));
    }

    #[test]
    fn checkpoint_missing_segment_fails() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let mut clip = stored_clip(&segment, 0, 1, 2);
        clip.segment_key = segment.key + 99;
        integrity_failure(write_checkpoint(&c, 1, "sh.600006", Period::Day, &[clip]).map(|_| ()));
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM version_checkpoint_clips WHERE version_key=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn checkpoint_bad_clip_fails() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let out_of_bounds = stored_clip(&segment, 0, 9, 3);
        integrity_failure(
            write_checkpoint(&c, 1, "sh.600006", Period::Day, &[out_of_bounds]).map(|_| ()),
        );
        let wrong_count = stored_clip(&segment, 0, 1, 3);
        integrity_failure(
            write_checkpoint(&c, 1, "sh.600006", Period::Day, &[wrong_count]).map(|_| ()),
        );
        let wrong_digest = StoredClip {
            segment_key: segment.key,
            clip: SegmentClip {
                segment_digest: [0xabu8; 32],
                clip_start_day: 0,
                clip_end_day: 1,
                row_count: 2,
            },
        };
        integrity_failure(
            write_checkpoint(&c, 1, "sh.600006", Period::Day, &[wrong_digest]).map(|_| ()),
        );
        let overlap = vec![
            stored_clip(&segment, 0, 1, 2),
            stored_clip(&segment, 1, 5, 2),
        ];
        integrity_failure(write_checkpoint(&c, 1, "sh.600006", Period::Day, &overlap).map(|_| ()));
    }

    #[test]
    fn checkpoint_tampered_rows_fail() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let clips = vec![stored_clip(&segment, 0, 5, 3)];
        write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).unwrap();
        c.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        c.execute(
            "UPDATE version_checkpoint_clips SET seq=7 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_checkpoint(&c, 1, "sh.600006", Period::Day).map(|_| ()));
        c.execute(
            "UPDATE version_checkpoint_clips SET seq=0,clip_end_day=99 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_checkpoint(&c, 1, "sh.600006", Period::Day).map(|_| ()));
        c.execute(
            "UPDATE version_checkpoint_clips SET clip_end_day=5,row_count=1 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_checkpoint(&c, 1, "sh.600006", Period::Day).map(|_| ()));
    }

    #[test]
    fn checkpoint_dangling_segment_reference_fails_on_read() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let clips = vec![stored_clip(&segment, 0, 5, 3)];
        write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let foreign_keys: i64 = c
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 0);
        c.execute(
            "DELETE FROM bar_segments WHERE segment_key=?1",
            params![segment.key],
        )
        .unwrap();
        integrity_failure(read_checkpoint(&c, 1, "sh.600006", Period::Day).map(|_| ()));
    }

    #[test]
    fn delta_write_read_roundtrip() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let parent = [0x11u8; 32];
        let ops = vec![
            StoredDelta {
                range_start_day: 0,
                range_end_day: 1,
                kind: StoredDeltaKind::Upsert(stored_clip(&segment, 0, 1, 2)),
            },
            StoredDelta {
                range_start_day: 2,
                range_end_day: 4,
                kind: StoredDeltaKind::Remove,
            },
            StoredDelta {
                range_start_day: 5,
                range_end_day: 5,
                kind: StoredDeltaKind::Upsert(stored_clip(&segment, 5, 5, 1)),
            },
        ];
        let (delta, projection) = write_delta(&c, 1, parent, &ops).unwrap();
        let (read_ops, delta_read, projection_read) = read_delta(&c, 1, parent).unwrap();
        assert_eq!(delta_read, delta);
        assert_eq!(projection_read, projection);
        assert_eq!(read_ops.len(), 3);
        let mut mismatched = parent;
        mismatched[0] ^= 1;
        let (_, _, wrong_projection) = read_delta(&c, 1, mismatched).unwrap();
        assert_ne!(wrong_projection, projection);
    }

    #[test]
    fn delta_malformed_rows_fail() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let ops = vec![StoredDelta {
            range_start_day: 0,
            range_end_day: 1,
            kind: StoredDeltaKind::Upsert(stored_clip(&segment, 0, 1, 2)),
        }];
        write_delta(&c, 1, [0x11u8; 32], &ops).unwrap();
        c.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        c.execute(
            "UPDATE version_delta_clips SET delta_kind=2 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
        c.execute(
            "UPDATE version_delta_clips SET delta_kind=1,row_count=0 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
        c.execute(
            "UPDATE version_delta_clips SET delta_kind=9,row_count=2 WHERE version_key=1",
            [],
        )
        .unwrap();
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
    }

    #[test]
    fn delta_tampered_seq_and_overlap_fail() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let ops = vec![
            StoredDelta {
                range_start_day: 0,
                range_end_day: 1,
                kind: StoredDeltaKind::Upsert(stored_clip(&segment, 0, 1, 2)),
            },
            StoredDelta {
                range_start_day: 2,
                range_end_day: 5,
                kind: StoredDeltaKind::Remove,
            },
        ];
        write_delta(&c, 1, [0x11u8; 32], &ops).unwrap();
        c.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        c.execute(
            "UPDATE version_delta_clips SET seq=5 WHERE version_key=1 AND seq=1",
            [],
        )
        .unwrap();
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
        c.execute(
            "UPDATE version_delta_clips SET seq=1 WHERE version_key=1 AND seq=5",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE version_delta_clips SET range_start_day=1 WHERE version_key=1 AND seq=1",
            [],
        )
        .unwrap();
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
    }

    #[test]
    fn delta_overlap_and_missing_segment_fail() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let overlap = vec![
            StoredDelta {
                range_start_day: 0,
                range_end_day: 2,
                kind: StoredDeltaKind::Upsert(stored_clip(&segment, 0, 1, 2)),
            },
            StoredDelta {
                range_start_day: 2,
                range_end_day: 5,
                kind: StoredDeltaKind::Remove,
            },
        ];
        integrity_failure(write_delta(&c, 1, [0x11u8; 32], &overlap).map(|_| ()));
        let dangling = StoredClip {
            segment_key: segment.key + 99,
            clip: stored_clip(&segment, 0, 1, 2).clip,
        };
        let ops = vec![StoredDelta {
            range_start_day: 0,
            range_end_day: 1,
            kind: StoredDeltaKind::Upsert(dangling),
        }];
        integrity_failure(write_delta(&c, 1, [0x11u8; 32], &ops).map(|_| ()));
        integrity_failure(read_delta(&c, 1, [0x11u8; 32]).map(|_| ()));
    }

    /// XP-011 round 2: the ctx path decodes and verifies each retained
    /// segment once — checkpoint validation + flatten hit the same entry,
    /// measured on the counters rather than inferred.
    #[test]
    fn segment_ctx_shares_verified_segment_between_readers() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let clips = vec![stored_clip(&segment, 0, 5, 3)];
        write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).unwrap();
        let mut ctx = SegmentReadCtx::default();
        let (read, digest) =
            read_checkpoint_ctx(&c, 1, "sh.600006", Period::Day, &mut ctx).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(ctx.stats().0, 1); // one full read_segment execution
        let flattened = super::compact_versions::flatten_clips_ctx(
            &c,
            "sh.600006",
            Period::Day,
            &read,
            &mut ctx,
        )
        .unwrap();
        assert_eq!(flattened.len(), 3);
        let (reads, hits, entries, _) = ctx.stats();
        assert_eq!(reads, 1);
        assert_eq!(hits, 1);
        assert_eq!(entries, 1);
        let _ = digest;
    }

    /// Corruption fails identically through the ctx and uncached reads,
    /// and a fresh context after the corruption still fails (retained
    /// proof never crosses transactions).
    #[test]
    fn segment_ctx_corruption_matches_uncached_and_fresh_ctx_fails() {
        let c = connection();
        let stored = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let mut ctx = SegmentReadCtx::default();
        read_segment_ctx(&c, stored.key, &mut ctx).unwrap();
        c.execute(
            "UPDATE segment_bars SET high_cents=high_cents+100 WHERE segment_key=?1 AND trade_day=1",
            params![stored.key],
        )
        .unwrap();
        // Same failure class as the uncached read.
        integrity_failure(read_segment(&c, stored.key));
        let mut fresh = SegmentReadCtx::default();
        integrity_failure(read_segment_ctx(&c, stored.key, &mut fresh).map(|_| ()));
        // Tampered stored digest also fails identically through the ctx.
        c.execute(
            "UPDATE bar_segments SET segment_digest=?1 WHERE segment_key=?2",
            params![[0xabu8; 32].as_slice(), stored.key],
        )
        .unwrap();
        integrity_failure(read_segment(&c, stored.key));
        let mut fresh2 = SegmentReadCtx::default();
        integrity_failure(read_segment_ctx(&c, stored.key, &mut fresh2).map(|_| ()));
    }

    /// Eviction past 128 entries and oversized payloads return identical
    /// facts — capacity bounds retention, never read correctness.
    #[test]
    fn segment_ctx_eviction_and_oversized_return_identical_facts() {
        let c = connection();
        let mut keys = Vec::new();
        for i in 0..130 {
            let unique = vec![CompactBar {
                trade_day: 100 + i,
                open_cents: i,
                high_cents: i + 10,
                low_cents: i - 10,
                close_cents: i,
                volume: None,
                amount: None,
                r7_raw: None,
            }];
            keys.push(
                insert_segment(&c, "sh.600006", Period::Day, &unique)
                    .unwrap()
                    .key,
            );
        }
        let mut ctx = SegmentReadCtx::default();
        for &key in &keys {
            read_segment_ctx(&c, key, &mut ctx).unwrap();
        }
        let (reads, _, entries, _) = ctx.stats();
        assert_eq!(reads, 130);
        assert_eq!(entries, 128);
        // The oldest entry was evicted: revisiting it re-reads and returns
        // content identical to the uncached read.
        let expected = read_segment(&c, keys[0]).unwrap();
        let again = read_segment_ctx(&c, keys[0], &mut ctx).unwrap();
        assert_eq!(*again, expected);
        assert_eq!(ctx.stats().0, 131);
        // The newest entry stayed retained — a hit, not a re-read.
        read_segment_ctx(&c, keys[129], &mut ctx).unwrap();
        assert_eq!(ctx.stats().0, 131);
        // An oversized segment is returned verified but not retained.
        let oversized = Rc::new(StoredSegment {
            key: 9999,
            digest: [0u8; 32],
            entity: "sh.600006".into(),
            period: Period::Day,
            bars: Vec::with_capacity(200_000),
        });
        ctx.retain(&oversized);
        let (_, _, entries, bytes) = ctx.stats();
        assert!(bytes <= SEGMENT_CTX_MAX_BYTES);
        assert!(!ctx.entries.contains_key(&9999) && entries <= 128);
        // ...and a normal read afterwards still succeeds.
        assert!(read_segment_ctx(&c, keys[0], &mut ctx).is_ok());
    }

    /// A context bound to one connection cannot serve another database:
    /// reusing it on a different `Connection` fails closed, and a fresh
    /// context on the second database verifies that database's own
    /// content.
    #[test]
    fn segment_ctx_is_bound_to_one_connection() {
        let c1 = connection();
        let stored = insert_segment(&c1, "sh.600006", Period::Day, &bars()).unwrap();
        let mut ctx = SegmentReadCtx::default();
        read_segment_ctx(&c1, stored.key, &mut ctx).unwrap();
        // Second database: same segment_key space, different content.
        let c2 = connection();
        let other = vec![CompactBar {
            trade_day: 7,
            open_cents: 1,
            high_cents: 2,
            low_cents: 1,
            close_cents: 1,
            volume: None,
            amount: None,
            r7_raw: None,
        }];
        let stored2 = insert_segment(&c2, "sh.600006", Period::Day, &other).unwrap();
        integrity_failure(read_segment_ctx(&c2, stored2.key, &mut ctx).map(|_| ()));
        let mut ctx2 = SegmentReadCtx::default();
        let got = read_segment_ctx(&c2, stored2.key, &mut ctx2).unwrap();
        assert_eq!(got.bars, other);
    }

    #[test]
    fn deterministic_digests_match_codec() {
        let c = connection();
        version(&c, 1);
        let segment = insert_segment(&c, "sh.600006", Period::Day, &bars()).unwrap();
        let clips = vec![stored_clip(&segment, 0, 5, 3)];
        let digest = write_checkpoint(&c, 1, "sh.600006", Period::Day, &clips).unwrap();
        let expected = compact_market::checkpoint_projection_digest(
            "sh.600006",
            Period::Day,
            &[SegmentClip {
                segment_digest: segment.digest,
                clip_start_day: 0,
                clip_end_day: 5,
                row_count: 3,
            }],
        )
        .unwrap();
        assert_eq!(digest, expected);
        let ops = vec![StoredDelta {
            range_start_day: 0,
            range_end_day: 5,
            kind: StoredDeltaKind::Upsert(stored_clip(&segment, 0, 5, 3)),
        }];
        version(&c, 2);
        let (delta, projection) = write_delta(&c, 2, [0x22u8; 32], &ops).unwrap();
        let (expected_delta, expected_projection) = compact_market::delta_projection_digest(
            [0x22u8; 32],
            &[DeltaOp {
                range_start_day: 0,
                range_end_day: 5,
                kind: DeltaKind::Upsert(clips[0].clip.clone()),
            }],
        )
        .unwrap();
        assert_eq!(delta, expected_delta);
        assert_eq!(projection, expected_projection);
    }
}
