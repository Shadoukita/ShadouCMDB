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

ARG RUST_VERSION=1.98
ARG NODE_VERSION=22

# --- Web UI (architecture-independent output) --------------------------------
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS ui
WORKDIR /src
COPY package.json package-lock.json ./
COPY frontend/package.json frontend/
COPY backend/package.json backend/
RUN npm ci --workspace frontend --include-workspace-root --no-audit --no-fund
COPY frontend frontend
# Until the frontend has a build script this yields an empty dist/, and the
# binary is built without a UI.
RUN npm run build --workspace frontend --if-present && mkdir -p frontend/dist

# --- Rust binary, cross-compiled for $TARGETARCH --------------------------------
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS build
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
COPY backend/rust backend/rust
WORKDIR /src/backend
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/backend/target,id=shadoucmdb-target-${TARGETARCH},sharing=locked \
    set -eu; . /build.env; \
    cargo build --release --locked --target "$TRIPLE"; \
    install -D -m 0755 "target/$TRIPLE/release/shadoucmdb" /out/shadoucmdb

# --- Runtime: glibc + CA certificates, no shell, non-root (uid 65532) ----------
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/shadoucmdb /usr/local/bin/shadoucmdb
ENV API_HOST=0.0.0.0 \
    API_PORT=3000
EXPOSE 3000
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/shadoucmdb"]
# Migrations are a separate, explicit step: `docker run ... shadoucmdb migrate`.
CMD ["serve"]
