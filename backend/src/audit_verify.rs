//! `shadoucmdb audit-verify`: checks the audit_log hash chain (migration 0018).
//!
//! Prints the chain head as `audit_log_chain_head` records it (sequence number
//! and hash). Compare it with the `rowHash` of the same `chainSeq` in the SIEM
//! (`AUDIT_EXPORT`): someone who can rewrite the whole table can also
//! recompute every hash in it, but not the copy that already left the host.
//!
//! Gaps fail unless a prune-audit run accounts for them (`retention`, accepted
//! with `--allow-gaps`); any other gap is `deleted` and always fails.

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
    // The head as the insert trigger recorded it (GH#511): the SIEM copy is
    // compared with this, and audit_log_verify() with the row it points to.
    let (rows, head_seq, head_hash): (i64, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM audit_log),
                (SELECT last_seq FROM audit_log_chain_head),
                (SELECT encode(last_hash, 'hex') FROM audit_log_chain_head)",
    )
    .fetch_one(pool)
    .await?;
    let problems =
        sqlx::query("SELECT chain_seq, audit_id, problem, detail FROM audit_log_verify()").fetch_all(pool).await?;

    println!("audit_log: {rows} rows");
    match (head_seq, head_hash) {
        (Some(seq), Some(hash)) if seq > 0 => println!("chain head: chainSeq {seq}, rowHash {hash}"),
        _ => println!("chain head: empty"),
    }
    let (mut broken, mut retention) = (0, 0);
    for p in &problems {
        let seq: i64 = p.try_get("chain_seq")?;
        let id: Option<i64> = p.try_get("audit_id")?;
        let problem: String = p.try_get("problem")?;
        let detail: String = p.try_get("detail")?;
        // Only a gap a prune-audit run accounts for may pass (GH#512); a
        // `deleted` gap fails like any other finding.
        if problem == "retention" {
            retention += 1;
        } else {
            broken += 1;
        }
        let id = id.map(|i| format!(" (id {i})")).unwrap_or_default();
        println!("  {problem:<9} chainSeq {seq}{id}: {detail}");
    }
    if broken > 0 {
        anyhow::bail!(
            "audit_log chain is broken: {broken} finding(s) (altered, relinked, deleted, missing tail or head mismatch)"
        );
    }
    if retention > 0 && !allow_gaps {
        anyhow::bail!(
            "audit_log has {retention} gap(s) left by prune-audit runs; re-run with --allow-gaps to accept them"
        );
    }
    if retention > 0 {
        println!("chain intact apart from {retention} gap(s) left by prune-audit runs (--allow-gaps)");
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
            vec![(
                n + 2,
                "deleted".into(),
                format!("rows {} to {} missing; no prune-audit run (audit.purge) accounts for them", n + 1, n + 1)
            )]
        );
        db.drop().await;
    }

    /// Runs `sql` as the table owner with the append-only trigger off, the
    /// way someone with the schema owner's rights edits the chain.
    async fn behind_the_trigger(pool: &sqlx::PgPool, sql: &str) {
        let mut c = pool.acquire().await.unwrap();
        let mut tx = c.begin().await.unwrap();
        tx.execute("ALTER TABLE audit_log DISABLE TRIGGER audit_log_append_only").await.unwrap();
        tx.execute(sqlx::AssertSqlSafe(sql.to_owned())).await.unwrap();
        tx.execute("ALTER TABLE audit_log ENABLE TRIGGER audit_log_append_only").await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Clears the log and resets the head, as on a database whose chain has
    /// no rows yet, so a test numbers its rows from 1. A migrated scratch
    /// database already holds rows dated now, which would sit before a test's
    /// back-dated ones.
    async fn empty_chain(pool: &sqlx::PgPool) {
        behind_the_trigger(
            pool,
            "DELETE FROM audit_log;
             UPDATE audit_log_chain_head SET last_seq = 0, last_hash = decode(repeat('00', 32), 'hex')",
        )
        .await;
    }

    /// `n` audit rows dated `days` ago, one transaction each.
    async fn insert_dated(pool: &sqlx::PgPool, n: usize, days: i32) {
        for _ in 0..n {
            sqlx::query(
                "INSERT INTO audit_log (occurred_at, actor_type, action, entity_type, entity_id, new_value)
                 VALUES (now() - make_interval(days => $1), 'system', 'create', 'lookup_list', gen_random_uuid(), '{}')",
            )
            .bind(days)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// GH#511: the newest rows rewritten and their hashes recomputed with
    /// cmdb.audit_log_hash() verify row by row; the head still holds the hash
    /// the trigger wrote, and audit-verify prints that one.
    #[tokio::test]
    async fn rewritten_newest_rows_with_recomputed_hashes_fail_on_the_head() {
        let Some(db) = scratch::database("rewritten_newest_rows_with_recomputed_hashes_fail_on_the_head").await else {
            return;
        };
        insert_in_one_transaction(&db.pool, 5).await;
        let (seq, written): (i64, String) =
            sqlx::query_as("SELECT last_seq, encode(last_hash, 'hex') FROM audit_log_chain_head")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        // Oldest first, each re-linked onto the rewritten row before it.
        let rehash = |s: i64| {
            format!(
                "UPDATE audit_log a SET new_value = '{{\"forged\": true}}',
                   prev_hash = p.row_hash,
                   row_hash = cmdb.audit_log_hash(p.row_hash, a.chain_seq, a.occurred_at, a.actor_type, a.actor_id,
                       a.actor_name, a.action, a.entity_type, a.entity_id, a.old_value,
                       '{{\"forged\": true}}'::jsonb, a.request_id)
                 FROM audit_log p WHERE a.chain_seq = {s} AND p.chain_seq = {s} - 1"
            )
        };
        behind_the_trigger(&db.pool, &format!("{}; {}", rehash(seq - 1), rehash(seq))).await;

        let found = problems(&db.pool).await;
        assert_eq!(
            found.iter().map(|(s, p, _)| (*s, p.as_str())).collect::<Vec<_>>(),
            vec![(seq, "head")],
            "{found:?}"
        );
        assert!(found[0].2.contains(&written), "the detail names the hash the trigger wrote: {found:?}");
        let err = super::check(&db.pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");
        db.drop().await;
    }

    /// GH#512: --allow-gaps accepts only gaps a prune-audit run accounts for.
    #[tokio::test]
    async fn only_a_prune_audit_run_excuses_a_gap() {
        let Some(db) = scratch::database("only_a_prune_audit_run_excuses_a_gap").await else { return };
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 3, 60).await; // chainSeq 1..=3, old enough to prune
        insert_dated(&db.pool, 3, 0).await; // 4..=6
        sqlx::query("SELECT * FROM prune_audit_log(interval '40 days', 'changes', false, 'test')")
            .execute(&db.pool)
            .await
            .unwrap(); // removes 1..=3, records audit.purge as 7
        assert_eq!(
            problems(&db.pool).await,
            vec![(
                4,
                "retention".into(),
                "rows 1 to 3 missing; pruned by the prune-audit run recorded at chainSeq 7".into()
            )]
        );
        let err = super::check(&db.pool, false).await.unwrap_err().to_string();
        assert!(err.contains("--allow-gaps"), "{err}");
        super::check(&db.pool, true).await.unwrap();

        // A recent row deleted behind the trigger: the purge does not cover it.
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 5").await;
        assert_eq!(
            problems(&db.pool).await.into_iter().map(|(s, p, _)| (s, p)).collect::<Vec<_>>(),
            vec![(4, "retention".into()), (6, "deleted".into())]
        );
        let err = super::check(&db.pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");

        // Nor does a later prune-audit run: the row before the gap is newer than its cutoff.
        insert_dated(&db.pool, 1, 0).await;
        sqlx::query("SELECT * FROM prune_audit_log(interval '30 days', 'changes', false, 'test')")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(problems(&db.pool).await.contains(&(
            6,
            "deleted".into(),
            "rows 5 to 5 missing; no prune-audit run (audit.purge) accounts for them".into()
        )));
        db.drop().await;
    }

    /// GH#512: an audit.purge row whose cutoff breaks the 30-day floor (one
    /// prune_audit_log() would never write) excuses nothing.
    #[tokio::test]
    async fn a_purge_row_inside_the_floor_does_not_excuse_a_gap() {
        let Some(db) = scratch::database("a_purge_row_inside_the_floor_does_not_excuse_a_gap").await else { return };
        insert_dated(&db.pool, 3, 0).await;
        sqlx::query(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
             VALUES ('system', 'forged', 'audit.purge', 'audit_log', gen_random_uuid(),
                     jsonb_build_object('cutoff', now() + interval '1 day'))",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 2").await;
        assert_eq!(
            problems(&db.pool).await.into_iter().map(|(s, p, _)| (s, p)).collect::<Vec<_>>(),
            vec![(3, "deleted".into())]
        );
        db.drop().await;
    }

    /// Problems as (chainSeq, problem), without the detail.
    async fn kinds(pool: &sqlx::PgPool) -> Vec<(i64, String)> {
        problems(pool).await.into_iter().map(|(s, p, _)| (s, p)).collect()
    }

    async fn prune(pool: &sqlx::PgPool, days: i32, scope: &str) {
        sqlx::query("SELECT * FROM prune_audit_log(make_interval(days => $1), $2, false, 'test')")
            .bind(days)
            .bind(scope)
            .execute(pool)
            .await
            .unwrap();
    }

    /// GH#683, case 1: a prune-audit run that deleted nothing does not excuse
    /// rows deleted from the start of the chain, where no row precedes the gap.
    #[tokio::test]
    async fn a_purge_that_deleted_nothing_does_not_excuse_the_oldest_rows() {
        let Some(db) = scratch::database("a_purge_that_deleted_nothing_does_not_excuse_the_oldest_rows").await else {
            return;
        };
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 5, 0).await; // 1..=5
        prune(&db.pool, 400, "auth").await; // deletes nothing, audit.purge as 6
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq IN (1, 2, 3)").await;
        assert_eq!(
            problems(&db.pool).await,
            vec![(
                4,
                "deleted".into(),
                "rows 1 to 3 missing; no prune-audit run (audit.purge) accounts for them".into()
            )]
        );
        let err = super::check(&db.pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");
        db.drop().await;
    }

    /// GH#683, case 2: recent rows deleted next to rows a prune-audit run
    /// removed are not covered by that run.
    #[tokio::test]
    async fn a_purge_does_not_excuse_recent_rows_deleted_next_to_its_own() {
        let Some(db) = scratch::database("a_purge_does_not_excuse_recent_rows_deleted_next_to_its_own").await else {
            return;
        };
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 3, 60).await; // 1..=3
        insert_dated(&db.pool, 3, 0).await; // 4..=6
        prune(&db.pool, 40, "changes").await; // removes 1..=3, audit.purge as 7
        insert_dated(&db.pool, 2, 0).await; // 8..=9
        super::check(&db.pool, true).await.unwrap();
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq IN (4, 5)").await;
        assert_eq!(kinds(&db.pool).await, vec![(6, "deleted".into())]);
        let err = super::check(&db.pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");
        db.drop().await;
    }

    /// GH#684: the API role cannot write an audit.purge row, and one dated in
    /// the future (or after the row that follows it) excuses nothing, even
    /// with ranges and counts that match the deleted rows.
    #[tokio::test]
    async fn a_forged_or_future_dated_purge_row_excuses_nothing() {
        let Some(roles) = scratch::Roles::create("a_forged_or_future_dated_purge_row_excuses_nothing").await else {
            return;
        };
        let db = roles.database().await;
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 3, 0).await; // 1..=3
        let forged = |at: &str| {
            format!(
                "INSERT INTO audit_log (occurred_at, actor_type, actor_name, action, entity_type, entity_id, new_value)
                 VALUES ({at}, 'system', 'shadoucmdb_maintenance', 'audit.purge', 'audit_log', gen_random_uuid(),
                         jsonb_build_object('scope', 'changes', 'cutoff', {at} - interval '31 days',
                                            'deleted', jsonb_build_object('create', 1), 'deletedRanges', '[[2, 2]]'::jsonb))"
            )
        };
        let api = roles.api_pool(&db).await;
        let err =
            sqlx::query(sqlx::AssertSqlSafe(forged("now() + interval '31 days'"))).execute(&api).await.unwrap_err();
        assert_eq!(err.as_database_error().and_then(|e| e.code()).as_deref(), Some("42501"), "{err}");
        api.close().await;

        // The same row with the owner's rights: in the future, it excuses nothing.
        sqlx::query(sqlx::AssertSqlSafe(forged("now() + interval '31 days'"))).execute(&db.pool).await.unwrap(); // 4
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 2").await;
        assert_eq!(kinds(&db.pool).await, vec![(3, "deleted".into())]);
        let err = super::check(&db.pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");

        // Dated after the row that follows it in the chain: nor does it. Until
        // that row is written, the same purge row (with the owner's rights,
        // which can delete rows anyway) covers row 2 of a fresh chain.
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 2, 60).await; // 1..=2
        sqlx::query(sqlx::AssertSqlSafe(forged("now()"))).execute(&db.pool).await.unwrap(); // 3
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 2").await;
        assert_eq!(kinds(&db.pool).await, vec![(3, "retention".into())]);
        insert_dated(&db.pool, 1, 2).await; // 4, dated two days before 3
        assert_eq!(kinds(&db.pool).await, vec![(3, "deleted".into())]);
        db.drop().await;
        roles.drop().await;
    }

    /// A genuine prune-audit run still verifies: interleaved scopes, two runs,
    /// several ranges each, and the purge rows' own records.
    #[tokio::test]
    async fn genuine_prune_audit_runs_still_verify() {
        let Some(db) = scratch::database("genuine_prune_audit_runs_still_verify").await else { return };
        empty_chain(&db.pool).await;
        for i in 0..12 {
            let action = if i % 3 == 0 { "login.failure" } else { "create" };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO audit_log (occurred_at, actor_type, action, entity_type, entity_id, new_value)
                 VALUES (now() - interval '200 days', 'system', '{action}', 'lookup_list', gen_random_uuid(), '{{}}')"
            )))
            .execute(&db.pool)
            .await
            .unwrap();
        }
        insert_dated(&db.pool, 2, 0).await; // 13..=14
        prune(&db.pool, 180, "auth").await; // 1, 4, 7, 10; audit.purge as 15
        let ranges: serde_json::Value = sqlx::query_scalar(
            "SELECT new_value->'deletedRanges' FROM audit_log WHERE action = 'audit.purge' ORDER BY chain_seq DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(ranges, serde_json::json!([[1, 1], [4, 4], [7, 7], [10, 10]]));
        super::check(&db.pool, true).await.unwrap();
        prune(&db.pool, 180, "changes").await; // the rest of 1..=12; audit.purge as 16
        assert_eq!(
            problems(&db.pool).await,
            vec![(
                13,
                "retention".into(),
                "rows 1 to 12 missing; pruned by the prune-audit run recorded at chainSeq 15, 16".into()
            )]
        );
        super::check(&db.pool, true).await.unwrap();
        db.drop().await;
    }

    /// Upgrade: gaps a prune-audit run left before migration 0060, whose
    /// audit.purge rows record no ranges, still verify through the legacy row
    /// it writes; a deletion after the upgrade does not.
    #[tokio::test]
    async fn retention_gaps_from_before_the_upgrade_still_verify() {
        let Some(db) = scratch::empty("retention_gaps_from_before_the_upgrade_still_verify").await else { return };
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                crate::db::MIGRATOR.iter().filter(|m| m.version <= 59).cloned().collect(),
            ),
            table_name: std::borrow::Cow::Borrowed("public._sqlx_migrations"),
            ..sqlx::migrate!("../sql/migrations")
        };
        before.run(&db.pool).await.unwrap();
        empty_chain(&db.pool).await;
        insert_dated(&db.pool, 3, 60).await; // 1..=3
        insert_dated(&db.pool, 3, 0).await; // 4..=6
        prune(&db.pool, 40, "changes").await; // 1..=3 removed; audit.purge as 7, no ranges
        insert_dated(&db.pool, 1, 0).await; // 8
        // A future-dated purge row only 0053 trusted, and the recent row it hid.
        sqlx::query(
            "INSERT INTO audit_log (occurred_at, actor_type, action, entity_type, entity_id, new_value)
             VALUES (now() + interval '31 days', 'system', 'audit.purge', 'audit_log', gen_random_uuid(),
                     jsonb_build_object('cutoff', now() + interval '1 day'))",
        )
        .execute(&db.pool)
        .await
        .unwrap(); // 9
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 5").await;
        assert_eq!(kinds(&db.pool).await, vec![(4, "retention".into()), (6, "retention".into())]);

        crate::db::MIGRATOR.run(&db.pool).await.unwrap();
        let legacy: serde_json::Value = sqlx::query_scalar(
            "SELECT new_value->'deletedRanges' FROM audit_log WHERE action = 'audit.purge' AND new_value->>'scope' = 'legacy'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(legacy, serde_json::json!([[1, 3]]), "only the gap a purge dated in the past accounts for");
        assert_eq!(kinds(&db.pool).await, vec![(4, "retention".into()), (6, "deleted".into())]);

        // Old rows deleted after the upgrade are not covered by the legacy row.
        behind_the_trigger(&db.pool, "DELETE FROM audit_log WHERE chain_seq = 4").await;
        assert_eq!(kinds(&db.pool).await, vec![(6, "deleted".into())]);
        db.drop().await;
    }

    /// Rows 1..=8 dated 200 days ago, 2..=4 and 6..=7 sign-in failures between
    /// changes; a recent row 9, then an `auth` prune-audit run recording
    /// [[2, 4], [6, 7]] as row 10, and a recent row 11.
    async fn two_pruned_ranges(pool: &sqlx::PgPool) {
        empty_chain(pool).await;
        let actions = [
            "create",
            "login.failure",
            "login.failure",
            "login.failure",
            "create",
            "login.failure",
            "login.failure",
            "create",
        ];
        for action in actions {
            sqlx::query(
                "INSERT INTO audit_log (occurred_at, actor_type, action, entity_type, entity_id, new_value)
                 VALUES (now() - interval '200 days', 'system', $1, 'lookup_list', gen_random_uuid(), '{}')",
            )
            .bind(action)
            .execute(pool)
            .await
            .unwrap();
        }
        insert_dated(pool, 1, 0).await; // 9
        prune(pool, 180, "auth").await; // 10
        insert_dated(pool, 1, 0).await; // 11
        let ranges: serde_json::Value =
            sqlx::query_scalar("SELECT new_value->'deletedRanges' FROM audit_log WHERE chain_seq = 10")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(ranges, serde_json::json!([[2, 4], [6, 7]]));
    }

    /// SHAA-2208: a row deleted by hand right next to a recorded range, on
    /// either side or between two ranges of the same run, is `deleted`; the
    /// run still accounts for its other range.
    #[tokio::test]
    async fn a_row_deleted_next_to_a_recorded_range_is_not_retention() {
        let Some(db) = scratch::database("a_row_deleted_next_to_a_recorded_range_is_not_retention").await else {
            return;
        };
        let pool = &db.pool;
        two_pruned_ranges(pool).await;
        assert_eq!(kinds(pool).await, vec![(5, "retention".into()), (8, "retention".into())]);
        super::check(pool, true).await.unwrap();

        // The row just before the first range.
        behind_the_trigger(pool, "DELETE FROM audit_log WHERE chain_seq = 1").await;
        assert_eq!(
            problems(pool).await,
            vec![
                (5, "deleted".into(), "rows 1 to 4 missing; no prune-audit run (audit.purge) accounts for them".into()),
                (
                    8,
                    "retention".into(),
                    "rows 6 to 7 missing; pruned by the prune-audit run recorded at chainSeq 10".into()
                ),
            ]
        );
        let err = super::check(pool, true).await.unwrap_err().to_string();
        assert!(err.contains("chain is broken"), "{err}");

        // The row between the two ranges: one gap, 2 to 7, not all of it recorded.
        two_pruned_ranges(pool).await;
        behind_the_trigger(pool, "DELETE FROM audit_log WHERE chain_seq = 5").await;
        assert_eq!(kinds(pool).await, vec![(8, "deleted".into())]);

        // The row just after the last range.
        two_pruned_ranges(pool).await;
        behind_the_trigger(pool, "DELETE FROM audit_log WHERE chain_seq = 8").await;
        assert_eq!(kinds(pool).await, vec![(5, "retention".into()), (9, "deleted".into())]);
        db.drop().await;
    }
}
