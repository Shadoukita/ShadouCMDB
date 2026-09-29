#!/usr/bin/env bash
# The OpenLDAP directory tools/ldap-it/ldap-it.ts runs against: LDAPS only,
# with a test CA made here. See README.md.
#
#   ldap.sh certs     test CA, server certificate (localhost, 127.0.0.1) and a second CA that did not sign it
#   ldap.sh start     start slapd and wait until it answers over verified TLS
#   ldap.sh seed      load seed.ldif (once, into an empty directory)
#   ldap.sh modify    apply the LDIF on stdin as the directory administrator (entries without changetype are added)
#   ldap.sh stop      stop slapd; `start` brings it back with its data
#   ldap.sh remove    stop and delete it
#
# LDAP_IT_MODE=docker (default) runs the image below as container ldap-it;
# LDAP_IT_MODE=local runs the slapd and ldapmodify on PATH (Debian/Ubuntu: apt install slapd ldap-utils;
# set LDAP_IT_SCHEMA and LDAP_IT_MODULES if they are not in /etc/ldap/schema and /usr/lib/ldap).
# LDAP_IT_DIR holds the certificates, configuration and (local) data; LDAP_IT_PORT is the LDAPS port
# on 127.0.0.1 and 127.0.0.2 (the certificate names only the first).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
MODE=${LDAP_IT_MODE:-docker}
DIR=${LDAP_IT_DIR:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/ldap-it}
PORT=${LDAP_IT_PORT:-6360}
# osixia/openldap 1.5.0 (OpenLDAP 2.4, multi-arch index): only its slapd, schema and modules are used,
# with the configuration below instead of the image's bootstrap.
IMAGE=osixia/openldap:1.5.0@sha256:18742e9c449c9c1afe129d3f2f3ee15fb34cc43e5f940a20f3399728f41d7c28
CONTAINER=ldap-it
ROOT_DN=cn=admin,dc=shadoucmdb,dc=test
ROOT_PW=ldap-it-admin-ci-only-password

certs() {
  mkdir -p "$DIR/certs"
  cd "$DIR/certs"
  rm -f ./*.pem ./*.srl ./*.csr
  local ca_ext='basicConstraints=critical,CA:TRUE'
  openssl req -x509 -new -noenc -newkey ec -pkeyopt ec_paramgen_curve:P-256 -days 2 -keyout ca-key.pem -out ca.pem \
    -subj '/CN=ShadouCMDB LDAP IT test CA' -addext "$ca_ext" -addext 'keyUsage=critical,keyCertSign,cRLSign' 2>/dev/null
  openssl req -new -noenc -newkey ec -pkeyopt ec_paramgen_curve:P-256 -keyout server-key.pem -out server.csr \
    -subj '/CN=localhost' 2>/dev/null
  printf '%s\n' 'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature' \
    'extendedKeyUsage=serverAuth' 'subjectAltName=DNS:localhost,IP:127.0.0.1' > server.ext
  openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -days 2 -out server.pem \
    -extfile server.ext 2>/dev/null
  # A CA that looks the same but did not sign the server certificate: "CA not trusted".
  openssl req -x509 -new -noenc -newkey ec -pkeyopt ec_paramgen_curve:P-256 -days 2 -keyout other-ca-key.pem \
    -out other-ca.pem -subj '/CN=ShadouCMDB LDAP IT test CA' -addext "$ca_ext" 2>/dev/null
  rm -f ca-key.pem other-ca-key.pem server.csr server.ext ca.srl
  # The container's slapd runs as root; a local one as this user.
  chmod 0644 ca.pem other-ca.pem server.pem
  chmod 0600 server-key.pem
  openssl verify -CAfile ca.pem server.pem
}

config() {
  local schema=$1 modules=$2 certs=$3 data=$4 run=$5
  mkdir -p "$DIR/conf"
  sed -e "s|@SCHEMA@|$schema|g" -e "s|@MODULES@|$modules|g" -e "s|@CERTS@|$certs|g" \
    -e "s|@DATA@|$data|g" -e "s|@RUN@|$run|g" "$HERE/slapd.conf.in" > "$DIR/conf/slapd.conf"
}

# Waits until the directory completes a TLS handshake verified against the test CA.
wait_ready() {
  for _ in $(seq 1 60); do
    if openssl s_client -connect "127.0.0.1:$PORT" -CAfile "$DIR/certs/ca.pem" -verify_return_error \
      -verify_hostname localhost </dev/null >/dev/null 2>&1; then
      echo "directory ready on ldaps://127.0.0.1:$PORT"
      return 0
    fi
    sleep 1
  done
  echo "directory did not come up on ldaps://127.0.0.1:$PORT" >&2
  logs >&2 || true
  return 1
}

logs() {
  if [ "$MODE" = docker ]; then docker logs --tail 50 "$CONTAINER"; else tail -50 "$DIR/slapd.log"; fi
}

start() {
  test -f "$DIR/certs/server.pem" || certs
  if [ "$MODE" = docker ]; then
    if docker container inspect "$CONTAINER" >/dev/null 2>&1; then
      docker start "$CONTAINER" >/dev/null
    else
      config /etc/ldap/schema /usr/lib/ldap /certs /tmp/ldap-it-data /tmp
      docker run -d --name "$CONTAINER" \
        -p "127.0.0.1:$PORT:636" -p "127.0.0.2:$PORT:636" \
        -v "$DIR/conf:/conf:ro" -v "$DIR/certs:/certs:ro" \
        --entrypoint /bin/sh "$IMAGE" \
        -c 'mkdir -p /tmp/ldap-it-data && exec /usr/sbin/slapd -d 256 -f /conf/slapd.conf -h ldaps:///' >/dev/null
    fi
  else
    mkdir -p "$DIR/data" "$DIR/run"
    config "${LDAP_IT_SCHEMA:-/etc/ldap/schema}" "${LDAP_IT_MODULES:-/usr/lib/ldap}" "$DIR/certs" "$DIR/data" "$DIR/run"
    "${LDAP_IT_SLAPD:-slapd}" -d 256 -f "$DIR/conf/slapd.conf" \
      -h "ldaps://127.0.0.1:$PORT/ ldaps://127.0.0.2:$PORT/" >>"$DIR/slapd.log" 2>&1 &
  fi
  wait_ready
}

stop() {
  if [ "$MODE" = docker ]; then
    docker stop "$CONTAINER" >/dev/null
  elif [ -f "$DIR/run/slapd.pid" ]; then
    local pid
    pid=$(cat "$DIR/run/slapd.pid")
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
  fi
  echo "directory stopped"
}

modify() {
  if [ "$MODE" = docker ]; then
    docker exec -i -e LDAPTLS_CACERT=/certs/ca.pem "$CONTAINER" \
      ldapmodify -a -H ldaps://localhost:636 -x -D "$ROOT_DN" -w "$ROOT_PW"
  else
    LDAPTLS_CACERT="$DIR/certs/ca.pem" "${LDAP_IT_LDAPMODIFY:-ldapmodify}" -a -H "ldaps://127.0.0.1:$PORT" \
      -x -D "$ROOT_DN" -w "$ROOT_PW"
  fi
}

remove() {
  if [ "$MODE" = docker ]; then
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  else
    stop
    rm -rf "$DIR/data" "$DIR/run"
  fi
}

case "${1:-}" in
  certs) certs ;;
  start) start ;;
  seed) modify < "$HERE/seed.ldif" ;;
  modify) modify ;;
  stop) stop ;;
  remove) remove ;;
  logs) logs ;;
  *) sed -n '2,17p' "$0" >&2; exit 2 ;;
esac
