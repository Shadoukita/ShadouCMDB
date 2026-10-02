# syntax=docker/dockerfile:1
#
# ShadouCMDB: one static-ish binary (API + migrations + embedded web UI) on a
# distroless base. Multi-arch: the Rust stage runs on the build machine and
# cross-compiles, so `--platform linux/amd64,linux/arm64` needs no emulation.
#
#   docker buildx build --platform linux/amd64,linux/arm64 -t shadoucmdb .
#   docker run --rm --env-file .env shadoucmdb migrate
#   docker run -d --env-file .env -p 3000:3000 shadoucmdb
#
# PostgreSQL is external: point DATABASE_URL / PG* (see .env.example) at it.

# Every base image is pinned by digest; Dependabot (docker ecosystem) bumps the
# tag and digest together. The tags are written out rather than taken from an
# ARG because Dependabot cannot resolve build arguments.

# --- Web UI (architecture-independent output) --------------------------------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c AS ui
WORKDIR /src
COPY package.json package-lock.json ./
COPY frontend/package.json frontend/
RUN npm ci --workspace frontend --include-workspace-root --no-audit --no-fund
COPY frontend frontend
# Until the frontend has a build script this yields an empty dist/, and the
# binary is built without a UI.
RUN npm run build --workspace frontend --if-present && mkdir -p frontend/dist

# --- Rust binary, cross-compiled for $TARGETARCH --------------------------------
FROM --platform=$BUILDPLATFORM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
ARG TARGETARCH
ARG BUILDARCH
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; gcc=x86_64-linux-gnu-gcc; pkg=gcc-x86-64-linux-gnu ;; \
      arm64) triple=aarch64-unknown-linux-gnu; gcc=aarch64-linux-gnu-gcc; pkg=gcc-aarch64-linux-gnu ;; \
      *) echo "unsupported TARGETARCH $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends "$pkg" libc6-dev-"$TARGETARCH"-cross && rm -rf /var/lib/apt/lists/*; \
    else gcc=gcc; fi; \
    rustup target add "$triple"; \
    upper=$(echo "$triple" | tr 'a-z-' 'A-Z_'); lower=$(echo "$triple" | tr '-' '_'); \
    printf 'TRIPLE=%s\nexport CARGO_TARGET_%s_LINKER=%s\nexport CC_%s=%s\n' "$triple" "$upper" "$gcc" "$lower" "$gcc" > /build.env
WORKDIR /src
COPY sql sql
COPY --from=ui /src/frontend/dist frontend/dist
COPY backend/Cargo.toml backend/Cargo.lock backend/build.rs backend/
COPY backend/.sqlx backend/.sqlx
COPY backend/src backend/src
WORKDIR /src/backend
# sqlx checks its queries against backend/.sqlx instead of a live database.
ENV SQLX_OFFLINE=true
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/backend/target,id=shadoucmdb-target-${TARGETARCH},sharing=locked \
    set -eu; . /build.env; \
    cargo build --release --locked --target "$TRIPLE"; \
    install -D -m 0755 "target/$TRIPLE/release/shadoucmdb" /out/shadoucmdb

# --- Runtime: glibc + CA certificates, no shell, non-root (uid 65532) ----------
# Pinned by digest; Dependabot (.github/dependabot.yml) proposes updates.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /out/shadoucmdb /usr/local/bin/shadoucmdb
ENV API_HOST=0.0.0.0 \
    API_PORT=3000
EXPOSE 3000
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/shadoucmdb"]
# Migrations are a separate, explicit step: `docker run ... shadoucmdb migrate`.
CMD ["serve"]
