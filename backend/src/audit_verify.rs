//! `shadoucmdb audit-verify`: checks the audit_log hash chain (migration 0007).
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
