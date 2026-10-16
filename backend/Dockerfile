# syntax=docker/dockerfile:1
#
# Build context for this Dockerfile MUST be the repository root (not backend/),
# because backend/Cargo.toml uses `[lints] workspace = true`, which requires the
# workspace root Cargo.toml (and the other workspace members' manifests) to be
# present during resolution. docker-compose.yml / docker-compose.override.yml
# are configured accordingly (context: ., dockerfile: backend/Dockerfile).
#
# Runtime environment variables (see .env.example):
#   ALLOWED_ORIGINS, MIN_CONTRACT_VERSION, DB_POOL_MIN, DB_POOL_MAX,
#   DB_POOL_TIMEOUT_SECS, NOTIFICATION_SCHEDULER_INTERVAL_SECS,
#   FCM_SERVER_KEY, FCM_PROJECT_ID, REMINDER_EMAIL_API_KEY, REMINDER_SMS_API_KEY,
#   REMINDER_ENCRYPTION_SECRET
# Additional vars read by the backend but not yet documented in .env.example:
#   NODE_ID, CONSENSUS_STRATEGY, REDIS_URL, ADMIN_API_KEY
# Database: the backend opens the SQLite file at DB_PATH (default
# /var/lib/ethos-protocol/ethos-protocol.db, a mounted volume). It refuses to
# start with DB_PATH=:memory: unless ALLOW_EPHEMERAL_DB=true is set (#690).

ARG RUST_VERSION=1.96.1

# ── Base: shared toolchain + system deps for building ───────────────────────
FROM rust:${RUST_VERSION}-slim-bookworm AS base
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc \
        pkg-config \
      && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# ── Development: live-reload against the bind-mounted backend/src ───────────
FROM base AS development
RUN cargo install cargo-watch --locked
COPY Cargo.toml Cargo.lock ./
COPY contracts ./contracts
COPY backend ./backend
EXPOSE 3000
CMD ["cargo", "watch", "--workdir", "/app", "-w", "backend/src", "-x", "run --package ethos-protocol-backend"]

# ── Builder: compile the release binary ──────────────────────────────────────
FROM base AS builder
COPY Cargo.toml Cargo.lock ./
COPY contracts ./contracts
COPY backend ./backend
RUN cargo build --release --package ethos-protocol-backend --locked

# ── Runtime: minimal production image ────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
      && rm -rf /var/lib/apt/lists/* \
      && useradd --system --create-home --shell /usr/sbin/nologin ethos

WORKDIR /app
COPY --from=builder /app/target/release/ethos-protocol-backend /usr/local/bin/ethos-protocol-backend

# SQLite database lives on a volume owned by the runtime user (#690). Mount a
# named volume here so data survives container restarts.
RUN mkdir -p /var/lib/ethos-protocol && chown ethos:ethos /var/lib/ethos-protocol
VOLUME ["/var/lib/ethos-protocol"]

USER ethos
EXPOSE 3000
ENV RUST_LOG=info \
    DB_PATH=/var/lib/ethos-protocol/ethos-protocol.db

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

ENTRYPOINT ["/usr/local/bin/ethos-protocol-backend"]
