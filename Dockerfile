# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim AS frontend-build
WORKDIR /workspace

COPY . .

RUN if [ -f frontend/package.json ]; then \
        cd frontend; \
        corepack enable; \
        if [ -f pnpm-lock.yaml ]; then \
            pnpm install --frozen-lockfile && pnpm run build; \
        elif [ -f yarn.lock ]; then \
            yarn install --frozen-lockfile && yarn build; \
        elif [ -f package-lock.json ]; then \
            npm ci && npm run build; \
        else \
            npm install && npm run build; \
        fi; \
    else \
        mkdir -p frontend/dist; \
    fi

FROM rust:1.96.0-bookworm AS rust-build
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
COPY crates ./crates
# conduit-db embeds the PostgreSQL migrations with include_str! at compile time.
COPY migrations ./migrations
COPY --from=frontend-build /workspace/frontend/dist ./frontend/dist

RUN cargo build --release -p conduit-bin --bin conduit-api --features redis

FROM debian:bookworm-slim AS runtime
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
