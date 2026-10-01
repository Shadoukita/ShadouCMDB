#!/bin/sh
# Business services performance gate (SHAA-927 spec §7.3): seeds the impact
# analysis data set (100 000 CIs, 300 000 relationships) plus 2 000 business
# services (one with 5 000 members, a CI in 300 services, chains nested 5
# deep) into a scratch database, measures the service list, a member page,
# adding 500 members and "part of", checks the query plans, and fails when a
# threshold is missed.
#
# Needs a PostgreSQL role that may create databases, as for the backend tests:
#   SHADOUCMDB_TEST_DATABASE_URL=postgres://user:password@host:5432/postgres tools/perf/business-services.sh
# Run it before a release, on the PostgreSQL version you support (16 in CI).
set -eu
: "${SHADOUCMDB_TEST_DATABASE_URL:?set SHADOUCMDB_TEST_DATABASE_URL (a role that may CREATE DATABASE)}"
cd "$(dirname "$0")/../../backend"
SQLX_OFFLINE=true exec cargo test --release business_services_performance -- --ignored --nocapture
