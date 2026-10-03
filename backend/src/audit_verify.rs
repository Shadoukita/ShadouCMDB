//! `shadoucmdb audit-verify`: checks the audit_log hash chain (migration 0018).
//!
//! Prints the chain head (sequence number and hash). Compare it with the
//! `rowHash` of the same `chainSeq` in the SIEM (`AUDIT_EXPORT`): someone who
//! can rewrite the whole table can also recompute every hash in it, but not
//! the copy that already left the host.

use sqlx::Row;

use crate::config::DatabaseConfig;
use crate::db;

pub async fn run(cfg: &DatabaseConfig, allow_gaps: bool) -> anyhow::Result<()> {
    let pool = db::connect(cfg).await?;
    let result = check(&pool, allow_gaps).await;
    pool.close().await;
    result
}

async fn check(pool: &sqlx::PgPool, allow_gaps: bool) -> anyhow::Result<()> {
    let (rows, head_seq, head_hash): (i64, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM audit_log),
                (SELECT chain_seq FROM audit_log ORDER BY chain_seq DESC LIMIT 1),
                (SELECT encode(row_hash, 'hex') FROM audit_log ORDER BY chain_seq DESC LIMIT 1)",
    )
    .fetch_one(pool)
    .await?;
    let problems =
        sqlx::query("SELECT chain_seq, audit_id, problem, detail FROM audit_log_verify()").fetch_all(pool).await?;

    println!("audit_log: {rows} rows");
    match (head_seq, head_hash) {
        (Some(seq), Some(hash)) => println!("chain head: chainSeq {seq}, rowHash {hash}"),
        _ => println!("chain head: empty"),
    }
    let mut altered = 0;
    for p in &problems {
        let seq: i64 = p.try_get("chain_seq")?;
        let id: Option<i64> = p.try_get("audit_id")?;
        let problem: String = p.try_get("problem")?;
        let detail: String = p.try_get("detail")?;
        if problem != "gap" {
            altered += 1;
        }
        let id = id.map(|i| format!(" (id {i})")).unwrap_or_default();
        println!("  {problem:<8} chainSeq {seq}{id}: {detail}");
    }
    let gaps = problems.len() - altered;
    if altered > 0 {
        anyhow::bail!("audit_log chain is broken: {altered} altered, relinked or missing-tail finding(s)");
    }
    if gaps > 0 && !allow_gaps {
        anyhow::bail!(
            "audit_log has {gaps} gap(s): rows were deleted. If retention pruning removed them, re-run with --allow-gaps"
        );
    }
    if gaps > 0 {
        println!("chain intact apart from {gaps} gap(s) (--allow-gaps)");
    } else {
        println!("chain intact");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::scratch;
    use sqlx::{Connection, Executor};

    /// One audit row per statement, as the API writes them.
    const INSERT_ONE: &str = "INSERT INTO audit_log (actor_type, action, entity_type, entity_id, new_value)
                              VALUES ('system', 'create', 'lookup_list', gen_random_uuid(), '{}')";

    async fn problems(pool: &sqlx::PgPool) -> Vec<(i64, String, String)> {
        sqlx::query_as("SELECT chain_seq, problem, detail FROM audit_log_verify()").fetch_all(pool).await.unwrap()
    }

    /// The head equals the newest row, and chain_seq runs 1..=n without holes.
    async fn assert_chain_intact(pool: &sqlx::PgPool) {
        assert_eq!(problems(pool).await, vec![]);
        let (n, max, head_ok): (i64, Option<i64>, bool) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM audit_log), (SELECT max(chain_seq) FROM audit_log),
                    (SELECT h.last_seq = a.chain_seq AND h.last_hash = a.row_hash
                     FROM audit_log_chain_head h,
                          (SELECT chain_seq, row_hash FROM audit_log ORDER BY chain_seq DESC LIMIT 1) a)",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(max, Some(n));
        assert!(head_ok, "the head is the newest row");
    }

    async fn insert_in_one_transaction(pool: &sqlx::PgPool, n: usize) {
        let mut tx = pool.begin().await.unwrap();
        for _ in 0..n {
            tx.execute(INSERT_ONE).await.unwrap();
        }
        tx.commit().await.unwrap();
    }

    /// Updates this backend has counted on the chain head and not yet flushed
    /// to the cumulative statistics. Since PostgreSQL 15 that includes earlier
    /// transactions on the same connection, but nothing is flushed while a
    /// transaction is open, so a difference taken inside one is exact.
    async fn head_updates(conn: &mut sqlx::PgConnection) -> i64 {
        sqlx::query_scalar(
            "SELECT n_tup_upd FROM pg_stat_xact_user_tables WHERE relid = 'audit_log_chain_head'::regclass",
        )
        .fetch_one(conn)
        .await
        .unwrap()
    }

    /// N single-row audit inserts in one transaction, looped on the server.
    /// Returns the head updates after the inserts and after the deferred
    /// commit trigger has fired (SET CONSTRAINTS ... IMMEDIATE runs it now,
    /// as COMMIT would).
    async fn head_updates_for(pool: &sqlx::PgPool, n: usize) -> (i64, i64) {
        let mut tx = pool.begin().await.unwrap();
        let before = head_updates(&mut tx).await;
        tx.execute(sqlx::AssertSqlSafe(format!("DO $$ BEGIN FOR i IN 1..{n} LOOP {INSERT_ONE}; END LOOP; END $$")))
            .await
            .unwrap();
        let after_inserts = head_updates(&mut tx).await - before;
        tx.execute("SET CONSTRAINTS ALL IMMEDIATE").await.unwrap();
        let at_commit = head_updates(&mut tx).await - before;
        tx.commit().await.unwrap();
        (after_inserts, at_commit)
    }

    /// GH#487: each row used to update the head, leaving a row version per
    /// audit row that every later insert in the transaction walked (O(N^2)).
    /// The head must move once per transaction, however many rows it writes.
    /// GH#526: counted, not timed, so parallel load cannot fail it.
    #[tokio::test]
    async fn audit_rows_in_one_transaction_update_the_head_once() {
        let Some(db) = scratch::database("audit_rows_in_one_transaction_update_the_head_once").await else { return };
        for n in [1, 2, 5_000] {
            assert_eq!(
                head_updates_for(&db.pool, n).await,
                (0, 1),
                "head updates (after inserts, at commit) for {n} rows"
            );
        }
        insert_in_one_transaction(&db.pool, 100).await;

        // Many rows in one statement see each other too.
        sqlx::query(
            "INSERT INTO audit_log (actor_type, action, entity_type, entity_id, new_value)
             SELECT 'system', 'create', 'lookup_list', gen_random_uuid(), jsonb_build_object('i', i)
             FROM generate_series(1, 500) i",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        assert_chain_intact(&db.pool).await;
        db.drop().await;
    }

    /// Writers in parallel, some rolling back to a savepoint or as a whole:
    /// the chain stays gapless and the head ends on the newest row.
    #[tokio::test]
    async fn concurrent_writers_keep_one_intact_chain() {
        let Some(db) = scratch::database("concurrent_writers_keep_one_intact_chain").await else { return };
        let mut writers = Vec::new();
        for w in 0..8 {
            let pool = db.pool.clone();
            writers.push(tokio::spawn(async move {
                for t in 0..10 {
                    let mut tx = pool.begin().await.unwrap();
                    for _ in 0..20 {
                        tx.execute(INSERT_ONE).await.unwrap();
                    }
                    let mut sp = tx.begin().await.unwrap();
                    sp.execute(INSERT_ONE).await.unwrap();
                    sp.rollback().await.unwrap();
                    tx.execute(INSERT_ONE).await.unwrap();
                    if (w + t) % 5 == 0 {
                        tx.rollback().await.unwrap();
                    } else {
                        tx.commit().await.unwrap();
                    }
                }
            }));
        }
        for w in writers {
            w.await.unwrap();
        }
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&db.pool).await.unwrap();
        assert!(n >= 8 * 8 * 21, "{n} rows");
        assert_chain_intact(&db.pool).await;
        db.drop().await;
    }

    /// Under REPEATABLE READ a writer whose snapshot predates another
    /// transaction's audit rows must fail, not fork the chain.
    #[tokio::test]
    async fn a_stale_repeatable_read_writer_gets_a_serialization_failure() {
        let Some(db) = scratch::database("a_stale_repeatable_read_writer_gets_a_serialization_failure").await else {
            return;
        };
        let mut stale = db.pool.acquire().await.unwrap();
        stale.execute("BEGIN ISOLATION LEVEL REPEATABLE READ").await.unwrap();
        stale.execute("SELECT count(*) FROM audit_log").await.unwrap(); // takes the snapshot
        insert_in_one_transaction(&db.pool, 3).await;
        let err = stale.execute(INSERT_ONE).await.unwrap_err();
        assert_eq!(err.as_database_error().and_then(|e| e.code()).as_deref(), Some("40001"), "{err}");
        stale.execute("ROLLBACK").await.unwrap();
        drop(stale);
        assert_chain_intact(&db.pool).await;
        db.drop().await;
    }

    /// Upgrade: a chain written by migration 0018's trigger continues under
    /// 0040's, and rows deleted from its end still show up.
    #[tokio::test]
    async fn the_upgrade_continues_an_existing_chain() {
        let Some(db) = scratch::empty("the_upgrade_continues_an_existing_chain").await else { return };
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                crate::db::MIGRATOR.iter().filter(|m| m.version <= 39).cloned().collect(),
            ),
            table_name: std::borrow::Cow::Borrowed("public._sqlx_migrations"),
            ..sqlx::migrate!("../sql/migrations")
        };
        before.run(&db.pool).await.unwrap();
        insert_in_one_transaction(&db.pool, 50).await;
        insert_in_one_transaction(&db.pool, 50).await;
        crate::db::MIGRATOR.run(&db.pool).await.unwrap();
        assert_chain_intact(&db.pool).await;
        insert_in_one_transaction(&db.pool, 50).await;
        assert_chain_intact(&db.pool).await;

        // The newest row deleted behind the trigger's back (the test role owns the table).
        let mut c = db.pool.acquire().await.unwrap();
        let mut tx = c.begin().await.unwrap();
        tx.execute("ALTER TABLE audit_log DISABLE TRIGGER audit_log_append_only").await.unwrap();
        tx.execute("DELETE FROM audit_log WHERE chain_seq = (SELECT max(chain_seq) FROM audit_log)").await.unwrap();
        tx.execute("ALTER TABLE audit_log ENABLE TRIGGER audit_log_append_only").await.unwrap();
        tx.commit().await.unwrap();
        drop(c);
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(&db.pool).await.unwrap();
        assert_eq!(
            problems(&db.pool).await,
            vec![(n + 1, "tail".into(), format!("rows {} to {} missing", n + 1, n + 1))]
        );
        // The chain continues from the head, not the newest surviving row.
        insert_in_one_transaction(&db.pool, 2).await;
        assert_eq!(
            problems(&db.pool).await,
            vec![(n + 2, "gap".into(), format!("rows {} to {} missing", n + 1, n + 1))]
        );
        db.drop().await;
    }
}
