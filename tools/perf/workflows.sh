#!/bin/sh
# Workflow performance gates (SHAA-1411 §9, SHAA-1424, SHAA-1698), each on a
# scratch database of 500 000 CIs:
# - runtime: every CI with a running workflow instance; measures transitions
#   (p95 target 50 ms), the instance list, the per-state summary, one CI's
#   workflows and one instance through the real router;
# - bootstrap: CIs without an instance; times the bootstrap dry run, the run
#   (batches of 1 000) and a second run, each within the default request
#   timeout of 120 s, then verifies the audit chain.
# Fails when a threshold is missed.
#
# Needs a PostgreSQL role that may create databases, as for the backend tests:
#   SHADOUCMDB_TEST_DATABASE_URL=postgres://user:password@host:5432/postgres tools/perf/workflows.sh
# Run it before a release, on the PostgreSQL version you support (16 in CI).
set -eu
: "${SHADOUCMDB_TEST_DATABASE_URL:?set SHADOUCMDB_TEST_DATABASE_URL (a role that may CREATE DATABASE)}"
cd "$(dirname "$0")/../../backend"
SQLX_OFFLINE=true exec cargo test --release modules::workflows::perf -- --ignored --nocapture --test-threads 1
