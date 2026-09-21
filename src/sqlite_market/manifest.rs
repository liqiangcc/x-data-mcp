//! Persistent version manifests. Each daily version's ordered record
//! projection is a height-balanced (AVL) tree of segment clips; interior and
//! leaf nodes are immutable rows shared by every version that references them.
//! A new version path-copies O(log n) nodes for its changed ranges instead of
//! restating inherited clips, so per-version manifest growth is independent of
//! history length and reconstruction depth stays logarithmic.
use super::*;
#[derive(Debug)]
struct Node {
    left: Option<i64>,
    right: Option<i64>,
    segment: Option<i64>,
    clip_start: Option<String>,
    clip_end: Option<String>,
    first: String,
    last: String,
    count: i64,
    height: i64,
}
fn get(c: &Connection, id: i64) -> Result<Node, Error> {
    let n = c
        .query_row(
            "SELECT left_id,right_id,segment_id,clip_start,clip_end,first_date,last_date,row_count,height FROM manifest_nodes WHERE node_id=?1",
            [id],
            |r| {
                Ok(Node {
                    left: r.get(0)?,
                    right: r.get(1)?,
                    segment: r.get(2)?,
                    clip_start: r.get(3)?,
                    clip_end: r.get(4)?,
                    first: r.get(5)?,
                    last: r.get(6)?,
                    count: r.get(7)?,
                    height: r.get(8)?,
                })
            },
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("manifest node missing"))?;
    // Node shape is fixed: a leaf carries exactly one segment clip, an
    // interior node carries exactly two children. Anything else is malformed.
    let leaf = n.segment.is_some();
    if leaf == (n.left.is_some() || n.right.is_some())
        || (!leaf && (n.left.is_none() || n.right.is_none()))
        || (leaf && (n.clip_start.is_none() || n.clip_end.is_none()))
    {
        return Err(integrity("manifest node malformed"));
    }
    Ok(n)
}
fn put(c: &Connection, n: &Node) -> Result<i64, Error> {
    c.execute(
        "INSERT INTO manifest_nodes VALUES(NULL,?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            n.left,
            n.right,
            n.segment,
            n.clip_start,
            n.clip_end,
            n.first,
            n.last,
            n.count,
            n.height
        ],
    )
    .map_err(storage)?;
    Ok(c.last_insert_rowid())
}
fn leaf(c: &Connection, segment: i64, start: &str, end: &str, count: i64) -> Result<i64, Error> {
    if count <= 0 {
        return Err(integrity("manifest leaf is empty"));
    }
    put(
        c,
        &Node {
            left: None,
            right: None,
            segment: Some(segment),
            clip_start: Some(start.to_owned()),
            clip_end: Some(end.to_owned()),
            first: start.to_owned(),
            last: end.to_owned(),
            count,
            height: 1,
        },
    )
}
fn mk(c: &Connection, left: i64, right: i64) -> Result<i64, Error> {
    let (l, r) = (get(c, left)?, get(c, right)?);
    put(
        c,
        &Node {
            left: Some(left),
            right: Some(right),
            segment: None,
            clip_start: None,
            clip_end: None,
            first: l.first,
            last: r.last,
            count: l.count + r.count,
            height: l.height.max(r.height) + 1,
        },
    )
}
/// AVL single/double rotation around a provisional (l,r) pair; every touched
/// node is a fresh row so existing trees stay immutable.
fn rebalance(c: &Connection, l: i64, r: i64) -> Result<i64, Error> {
    let (ln, rn) = (get(c, l)?, get(c, r)?);
    if ln.height > rn.height + 1 {
        let (ll, lr) = (
            ln.left
                .ok_or_else(|| integrity("manifest node malformed"))?,
            ln.right
                .ok_or_else(|| integrity("manifest node malformed"))?,
        );
        let lrn = get(c, lr)?;
        if lrn.height > get(c, ll)?.height {
            let (lrl, lrr) = (
                lrn.left
                    .ok_or_else(|| integrity("manifest node malformed"))?,
                lrn.right
                    .ok_or_else(|| integrity("manifest node malformed"))?,
            );
            let a = mk(c, ll, lrl)?;
            let b = mk(c, lrr, r)?;
            mk(c, a, b)
        } else {
            let b = mk(c, lr, r)?;
            mk(c, ll, b)
        }
    } else if rn.height > ln.height + 1 {
        let (rl, rr) = (
            rn.left
                .ok_or_else(|| integrity("manifest node malformed"))?,
            rn.right
                .ok_or_else(|| integrity("manifest node malformed"))?,
        );
        let rln = get(c, rl)?;
        if rln.height > get(c, rr)?.height {
            let (rll, rlr) = (
                rln.left
                    .ok_or_else(|| integrity("manifest node malformed"))?,
                rln.right
                    .ok_or_else(|| integrity("manifest node malformed"))?,
            );
            let a = mk(c, l, rll)?;
            let b = mk(c, rlr, rr)?;
            mk(c, a, b)
        } else {
            let a = mk(c, l, rl)?;
            mk(c, a, rr)
        }
    } else {
        mk(c, l, r)
    }
}
/// Ordered rope concat: every record under `l` precedes every record under `r`.
fn concat(c: &Connection, l: i64, r: i64) -> Result<i64, Error> {
    let (ln, rn) = (get(c, l)?, get(c, r)?);
    if ln.height > rn.height + 1 {
        let right = concat(
            c,
            ln.right
                .ok_or_else(|| integrity("manifest node malformed"))?,
            r,
        )?;
        return rebalance(
            c,
            ln.left
                .ok_or_else(|| integrity("manifest node malformed"))?,
            right,
        );
    }
    if rn.height > ln.height + 1 {
        let left = concat(
            c,
            l,
            rn.left
                .ok_or_else(|| integrity("manifest node malformed"))?,
        )?;
        return rebalance(
            c,
            left,
            rn.right
                .ok_or_else(|| integrity("manifest node malformed"))?,
        );
    }
    mk(c, l, r)
}
fn join(c: &Connection, l: Option<i64>, r: Option<i64>) -> Result<Option<i64>, Error> {
    match (l, r) {
        (Some(l), Some(r)) => Ok(Some(concat(c, l, r)?)),
        (x, None) | (None, x) => Ok(x),
    }
}
/// Extract the base subtree covering records [lo,hi] (inclusive, 0-indexed).
/// Whole subtrees are reused by reference; only boundary leaf slices and the
/// join of two partial children write new nodes, so no orphan complement
/// tree is ever materialized.
fn slice(c: &Connection, root: i64, lo: i64, hi: i64) -> Result<i64, Error> {
    let n = get(c, root)?;
    if lo > hi || lo < 0 || hi >= n.count {
        return Err(integrity("manifest slice range out of bounds"));
    }
    if lo == 0 && hi == n.count - 1 {
        return Ok(root);
    }
    if n.segment.is_some() {
        // The boundary dates come from the segment rows themselves, so a
        // corrupt clip bound fails closed here.
        let (seg, cs, ce) = (
            n.segment
                .ok_or_else(|| integrity("manifest leaf malformed"))?,
            n.clip_start
                .as_deref()
                .ok_or_else(|| integrity("manifest leaf malformed"))?,
            n.clip_end
                .as_deref()
                .ok_or_else(|| integrity("manifest leaf malformed"))?,
        );
        let date_at = |off: i64| -> Result<String, Error> {
            c.query_row(
                "SELECT trade_date FROM segment_bars WHERE segment_id=?1 AND trade_date>=?2 AND trade_date<=?3 ORDER BY trade_date LIMIT 1 OFFSET ?4",
                params![seg, cs, ce, off],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| integrity("manifest clip length mismatch"))
        };
        return leaf(c, seg, &date_at(lo)?, &date_at(hi)?, hi - lo + 1);
    }
    let (l, r) = (
        n.left.ok_or_else(|| integrity("manifest node malformed"))?,
        n.right
            .ok_or_else(|| integrity("manifest node malformed"))?,
    );
    let lc = get(c, l)?.count;
    let a = if lo < lc {
        Some(slice(c, l, lo, hi.min(lc - 1))?)
    } else {
        None
    };
    let b = if hi >= lc {
        Some(slice(c, r, lo.max(lc) - lc, hi - lc)?)
    } else {
        None
    };
    match (a, b) {
        (Some(x), None) | (None, Some(x)) => Ok(x),
        (Some(a), Some(b)) => concat(c, a, b),
        (None, None) => Err(integrity("manifest slice empty")),
    }
}
fn build_balanced(c: &Connection, leaves: &[i64]) -> Result<Option<i64>, Error> {
    match leaves.len() {
        0 => Ok(None),
        1 => Ok(Some(leaves[0])),
        n => {
            let a = build_balanced(c, &leaves[..n / 2])?;
            let b = build_balanced(c, &leaves[n / 2..])?;
            join(c, a, b)
        }
    }
}
/// One contiguous run of the new version's records.
pub(crate) enum Piece {
    /// Records equal to the base version's positions [first,last]; shared
    /// subtrees are extracted from the base manifest instead of restated.
    Base { first: usize, last: usize },
    /// Newly materialized bars held by `segment` between first_date/last_date.
    New {
        segment: i64,
        first_date: String,
        last_date: String,
        count: usize,
    },
}
/// Splice `pieces` against the base version's manifest. Every `Base` run keeps
/// the parent's subtree; every `New` run contributes one fresh leaf. Writes
/// O(log n) nodes per changed region and returns the new root.
pub(crate) fn build(
    c: &Connection,
    base_root: Option<i64>,
    pieces: &[Piece],
) -> Result<i64, Error> {
    let mut trees: Vec<i64> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match piece {
            Piece::Base { first, last } => {
                let root = base_root.ok_or_else(|| integrity("manifest base absent"))?;
                trees.push(slice(c, root, *first as i64, *last as i64)?);
            }
            Piece::New {
                segment,
                first_date,
                last_date,
                count,
            } => trees.push(leaf(c, *segment, first_date, last_date, *count as i64)?),
        }
    }
    let root = build_balanced(c, &trees)?.ok_or_else(|| integrity("manifest empty"))?;
    Ok(root)
}
/// Ordered leaf clips intersecting [start,end]; interior nodes outside the
/// range are pruned by their stored first/last bounds.
pub(crate) fn clips_in_range(
    c: &Connection,
    root: i64,
    start: &str,
    end: &str,
) -> Result<Vec<(i64, String, String)>, Error> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let n = get(c, id)?;
        if n.last.as_str() < start || n.first.as_str() > end {
            continue;
        }
        if let Some(seg) = n.segment {
            let (cs, ce) = (
                n.clip_start
                    .ok_or_else(|| integrity("manifest leaf malformed"))?,
                n.clip_end
                    .ok_or_else(|| integrity("manifest leaf malformed"))?,
            );
            out.push((seg, cs.max(start.to_owned()), ce.min(end.to_owned())));
        } else {
            // Stack is LIFO: push right first so left is visited first.
            stack.push(
                n.right
                    .ok_or_else(|| integrity("manifest node malformed"))?,
            );
            stack.push(n.left.ok_or_else(|| integrity("manifest node malformed"))?);
        }
    }
    Ok(out)
}
/// In-order parsed bars for the whole manifest. Each leaf's fetched row count
/// must equal its declared length, so clip damage fails closed before the
/// caller's digest check.
pub(crate) fn bars(c: &Connection, root: i64) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut order = Vec::new();
    while let Some(id) = stack.pop() {
        let n = get(c, id)?;
        if n.segment.is_some() {
            order.push(n);
        } else {
            stack.push(
                n.right
                    .ok_or_else(|| integrity("manifest node malformed"))?,
            );
            stack.push(n.left.ok_or_else(|| integrity("manifest node malformed"))?);
        }
    }
    for n in order {
        let mut stmt = c
            .prepare(
                "SELECT bar_json FROM segment_bars WHERE segment_id=?1 AND trade_date>=?2 AND trade_date<=?3 ORDER BY trade_date",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map(
                params![
                    n.segment.unwrap(),
                    n.clip_start.unwrap(),
                    n.clip_end.unwrap()
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        if rows.len() as i64 != n.count {
            return Err(integrity("manifest clip length mismatch"));
        }
        for j in rows {
            out.push(serde_json::from_str(&j).map_err(integrity)?);
        }
    }
    Ok(out)
}
/// Root node id stored on the version row.
pub(crate) fn root_of(c: &Connection, version_id: &str) -> Result<Option<i64>, Error> {
    let root: Option<Option<i64>> = c
        .query_row(
            "SELECT manifest_root FROM data_versions WHERE version_id=?1",
            [version_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    Ok(root.flatten())
}
fn evidence_digest_of(c: &Connection, version_id: &str) -> Result<String, Error> {
    let env: String = c
        .query_row(
            "SELECT envelope_json FROM data_versions WHERE version_id=?1",
            [version_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| integrity("version missing during manifest migration"))?;
    let env: Value = serde_json::from_str(&env).map_err(integrity)?;
    let raw = crate::sqlite_market::envelope::expand(c, Some(version_id), &env)?;
    let record = compat::check(&raw)?;
    Ok(crate::hash::sha256(
        text(&serde_json::json!(evidence(&record)?))?.as_bytes(),
    ))
}
/// Post-migration data step for schema 017: convert every version's flat
/// `version_segments` clips into a shared manifest tree, seal the evidence
/// projection into `evidence_digest`, then drop the superseded tables. Runs
/// inside the schema transaction; any failure rolls back.
pub(crate) fn migrate(c: &Connection) -> Result<(), Error> {
    // `version_segments` is dropped by this migration; on a rerun the clip
    // conversion is skipped and only evidence backfill/drop execute.
    let clips_present: bool = c
        .query_row(
            "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name='version_segments'",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if clips_present {
        migrate_clips(c)?;
    }
    // Every segment-backed envelope must now resolve through a manifest.
    let unmanifested: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM data_versions WHERE json_extract(envelope_json,'$.records.$ref')='segments' AND manifest_root IS NULL)",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if unmanifested {
        return Err(integrity("segment-backed version lacks manifest"));
    }
    let mut stmt = c
        .prepare(
            "SELECT version_id FROM data_versions WHERE evidence_digest IS NULL ORDER BY rowid",
        )
        .map_err(storage)?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    for id in ids {
        c.execute(
            "UPDATE data_versions SET evidence_digest=?1 WHERE version_id=?2",
            params![evidence_digest_of(c, &id)?, id],
        )
        .map_err(storage)?;
    }
    c.execute_batch("DROP TABLE IF EXISTS version_segments;")
        .map_err(storage)?;
    Ok(())
}
fn migrate_clips(c: &Connection) -> Result<(), Error> {
    let mut stmt = c
        .prepare(
            "SELECT vs.version_id,vs.segment_id,vs.clip_start,vs.clip_end \
             FROM version_segments vs JOIN data_versions v ON v.version_id=vs.version_id \
             ORDER BY v.rowid,vs.seq",
        )
        .map_err(storage)?;
    let clips = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    drop(stmt);
    let mut current: Option<(String, Vec<i64>)> = None;
    let flush = |entry: Option<(String, Vec<i64>)>| -> Result<(), Error> {
        let Some((id, leaves)) = entry else {
            return Ok(());
        };
        let root =
            build_balanced(c, &leaves)?.ok_or_else(|| integrity("migrated manifest empty"))?;
        c.execute(
            "UPDATE data_versions SET manifest_root=?1 WHERE version_id=?2",
            params![root, id],
        )
        .map_err(storage)?;
        Ok(())
    };
    for (id, seg, cs, ce) in clips {
        if current.as_ref().map(|(v, _)| v != &id).unwrap_or(true) {
            flush(current.take())?;
            current = Some((id.clone(), Vec::new()));
        }
        let count: i64 = c
            .query_row(
                "SELECT count(*) FROM segment_bars WHERE segment_id=?1 AND trade_date>=?2 AND trade_date<=?3",
                params![seg, cs, ce],
                |r| r.get(0),
            )
            .map_err(storage)?;
        current
            .as_mut()
            .unwrap()
            .1
            .push(leaf(c, seg, &cs, &ce, count)?);
    }
    flush(current.take())?;
    Ok(())
}
