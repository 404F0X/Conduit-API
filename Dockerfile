# syntax=docker/dockerfile:1

FROM node:25-bookworm-slim@sha256:81db02c4b671288a03915da9534dbd54f96d0e7c24d80ccc54f5b36b2e684370 AS frontend-build
WORKDIR /workspace

COPY frontend/package.json frontend/pnpm-lock.yaml ./frontend/
RUN corepack enable \
    && cd frontend \
    && HUSKY=0 pnpm install --frozen-lockfile

COPY frontend ./frontend
RUN cd frontend && pnpm run build

FROM rust:1.96.0-bookworm@sha256:5e2214abe154fe26e39f64488952e5c991eeed1d6d6da7cc8381ae83927f0cfc AS rust-build
WORKDIR /workspace

# PostgreSQL support is compiled into the workspace; there is no selectable
# database feature axis in release builds. Redis remains runtime-optional, but
# the production image includes its feature so redis/two-level cache modes are
# usable without a separate image variant.

# --- S07: build-args → build-info injection -----------------------------------
# Consumed by crates/conduit-bin/build.rs (override_or). Defaults keep the build
# reproducible when the args are not supplied.
ARG CONDUIT_VERSION=""
ARG CONDUIT_COMMIT=""
ARG CONDUIT_BUILD_TIME=""
ARG CONDUIT_BRANCH=""
ENV CONDUIT_VERSION=${CONDUIT_VERSION} \
    CONDUIT_COMMIT=${CONDUIT_COMMIT} \
    CONDUIT_BUILD_TIME=${CONDUIT_BUILD_TIME} \
    CONDUIT_BRANCH=${CONDUIT_BRANCH}

COPY Cargo.toml Cargo.lock ./
COPY config.example.yml ./
COPY crates ./crates
# conduit-db embeds the PostgreSQL migrations with include_str! at compile time.
COPY migrations ./migrations

RUN cargo build --locked --release -p conduit-bin --bin conduit-api --features redis

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS runtime
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata curl \
    && rm -rf /var/lib/apt/lists/* \
    # Runtime runs as non-root user `conduit` (UID 10001). `/data` remains
    # available for explicitly configured filesystem object storage;
    # PostgreSQL data lives in its own service.
    && groupadd --system --gid 10001 conduit \
    && useradd --system --uid 10001 --gid conduit --home-dir /app --shell /usr/sbin/nologin conduit \
    && mkdir -p /data /app \
    && chown -R conduit:conduit /data /app

COPY --from=rust-build --chown=conduit:conduit /workspace/target/release/conduit-api /app/conduit-api
COPY --from=frontend-build --chown=conduit:conduit /workspace/frontend/dist /app/frontend/dist
COPY --chown=conduit:conduit LICENSE NOTICE /app/licenses/
COPY --chown=conduit:conduit LICENSES /app/licenses/third-party/

USER conduit

# A DSN is deliberately not baked into the image. Compose/Kubernetes must
# provide CONDUIT_DB_DSN from deployment configuration or a secret store.
ENV CONDUIT_DB_DIALECT=postgres \
    CONDUIT_SERVER_HOST=0.0.0.0 \
    CONDUIT_SERVER_PORT=8090 \
    CONDUIT_LOG_STDOUT=true

EXPOSE 8090 9090

STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS "http://127.0.0.1:${CONDUIT_SERVER_PORT:-8090}/health" >/dev/null \
        && curl -fsS "http://127.0.0.1:${CONDUIT_SERVER_PORT:-8090}/admin/system/status" >/dev/null \
        || exit 1

ENTRYPOINT ["/app/conduit-api"]
