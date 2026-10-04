#!/bin/sh
# Workflow runtime performance gate (SHAA-1411 §9, SHAA-1424): seeds 500 000
# CIs, each with a running workflow instance, into a scratch database, then
# measures transitions (p95 target 50 ms), the instance list, the per-state
# summary, one CI's workflows and one instance through the real router, and
# fails when a threshold is missed.
#
# Needs a PostgreSQL role that may create databases, as for the backend tests:
#   SHADOUCMDB_TEST_DATABASE_URL=postgres://user:password@host:5432/postgres tools/perf/workflows.sh
# Run it before a release, on the PostgreSQL version you support (16 in CI).
set -eu
: "${SHADOUCMDB_TEST_DATABASE_URL:?set SHADOUCMDB_TEST_DATABASE_URL (a role that may CREATE DATABASE)}"
cd "$(dirname "$0")/../../backend"
SQLX_OFFLINE=true exec cargo test --release workflow_runtime_performance -- --ignored --nocapture
