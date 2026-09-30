#!/bin/sh
# Impact analysis performance gate (SHAA-883 spec §6.2): seeds 100 000 CIs and
# 300 000 relationships into a scratch database, measures the analysis at the
# defaults and at the maximum, the 50 000-relationship hub, 16 parallel
# callers and the hop query plans, and fails when a threshold is missed.
#
# Needs a PostgreSQL role that may create databases, as for the backend tests:
#   SHADOUCMDB_TEST_DATABASE_URL=postgres://user:password@host:5432/postgres tools/perf/impact.sh
# Run it before a release, on the PostgreSQL version you support (16 in CI).
set -eu
: "${SHADOUCMDB_TEST_DATABASE_URL:?set SHADOUCMDB_TEST_DATABASE_URL (a role that may CREATE DATABASE)}"
cd "$(dirname "$0")/../../backend"
SQLX_OFFLINE=true exec cargo test --release impact_performance -- --ignored --nocapture
