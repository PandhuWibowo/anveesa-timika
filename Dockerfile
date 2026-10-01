# Single image: Rust backend serving the built Svelte SPA from ./static.

FROM oven/bun:1 AS web
WORKDIR /web
COPY frontend/package.json frontend/bun.lock* ./
RUN bun install --frozen-lockfile || bun install
COPY frontend/ ./
# gRPC-Web client code from the .proto contract (buf.gen.yaml reads ../proto).
COPY proto/ /proto/
RUN bun run gen && bun run build

# Pin the builder to the runtime's Debian release so the binary's glibc matches.
FROM rust:1-slim-bookworm AS api
WORKDIR /api
COPY backend/Cargo.toml backend/Cargo.lock* backend/build.rs ./
# build.rs compiles ../proto (tonic-build, vendored protoc).
COPY proto/ /proto/
# Build dependencies against a stub first so source edits don't recompile them.
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY backend/src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app timika \
 && mkdir -p /data /var/log/timika && chown timika /data /var/log/timika
WORKDIR /app
COPY --from=api /api/target/release/anveesa-timika-backend /app/timika
COPY --from=web /web/dist /app/static
# API on 8200, Raft cluster traffic on 8201. Raft data lives in /data, logs in
# /var/log/timika (mount volumes on both). Logging defaults: operational log files
# + audit log on; set LOG_DIR= / AUDIT_FILE= empty to disable (docs/LOGGING.md).
ENV BIND_ADDR=0.0.0.0:8200 \
    CLUSTER_BIND_ADDR=0.0.0.0:8201 \
    RAFT_PATH=/data \
    LOG_DIR=/var/log/timika \
    AUDIT_FILE=/var/log/timika/audit-{instance}.log
EXPOSE 8200 8201
VOLUME ["/data", "/var/log/timika"]
USER 10001
# Liveness only: a SEALED instance is healthy (it's waiting for operators).
# Never make this check unseal-dependent, or Swarm/compose would kill sealed
# replicas in a loop. Traffic routing on seal state is the load balancer's job.
HEALTHCHECK --interval=10s --timeout=4s --start-period=10s --retries=3 \
  CMD ["/app/timika", "operator", "health"]
CMD ["/app/timika"]
