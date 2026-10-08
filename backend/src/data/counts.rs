//! Count histories (dashboard KPIs, SHAA-2350): how many rows counted at each
//! point of a time range, derived from the timestamps the rows already carry.
//!
//! Each row counts during one span from `s` (created, or entered its validity
//! period) to `e` (deleted, or left its validity period; NULL while it still
//! counts). A row with `e <= s` never counts. The count at an instant `t` is
//! the rows with `s < t <= e` (or `e` NULL): a row created exactly at the start
//! of a bucket is added in that bucket, and the count at the end of a bucket is
//! the count at its start plus what it added minus what it removed.
//!
//! No snapshot table: one pass over the source table answers any range, the
//! numbers agree with the inventory list, and nothing needs a scheduler. What
//! the timestamps do not keep is not counted: a restored row counts as if it
//! had never been deleted, and a purged row is gone from the past as well.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Postgres, QueryBuilder};

/// Rows that started (`added`) and stopped (`removed`) counting in one bucket.
#[derive(Debug, sqlx::FromRow)]
pub struct CountEvents {
    /// None: the row carrying the count at `from` in `added`
    pub start: Option<DateTime<Utc>>,
    pub added: i64,
    pub removed: i64,
}

/// The count at `from`, and the non-empty buckets of `[from, to)` per `unit`
/// ('day' or 'week': ISO weeks, Monday 00:00 UTC), in order. `spans` pushes a
/// query selecting the columns `s` and `e` of every row that may count.
pub async fn count_history(
    conn: &mut PgConnection,
    spans: impl FnOnce(&mut QueryBuilder<Postgres>),
    unit: &'static str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> sqlx::Result<(i64, Vec<CountEvents>)> {
    let mut qb = QueryBuilder::<Postgres>::new("WITH x AS MATERIALIZED (SELECT s, e FROM (");
    spans(&mut qb);
    qb.push(
        ") spans WHERE e IS NULL OR e > s), \
         ev AS (SELECT s AS t, 1 AS a, 0 AS r FROM x UNION ALL SELECT e, 0, 1 FROM x WHERE e IS NOT NULL) ",
    );
    qb.push("SELECT NULL::timestamptz AS start, count(*) AS added, 0::bigint AS removed FROM x WHERE s < ");
    qb.push_bind(from).push(" AND (e IS NULL OR e >= ").push_bind(from).push(")");
    qb.push(format!(
        " UNION ALL SELECT date_trunc('{unit}', t AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', \
         sum(a)::bigint, sum(r)::bigint FROM ev WHERE t >= "
    ));
    qb.push_bind(from).push(" AND t < ").push_bind(to).push(" GROUP BY 1 ORDER BY 1 NULLS FIRST");
    let mut rows: Vec<CountEvents> = qb.build_query_as().persistent(false).fetch_all(&mut *conn).await?;
    let at_from = match rows.first() {
        Some(CountEvents { start: None, added, .. }) => *added,
        _ => 0,
    };
    rows.retain(|r| r.start.is_some());
    Ok((at_from, rows))
}
