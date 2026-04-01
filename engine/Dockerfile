# ── Build stage ────────────────────────────────────────────────────────────
FROM rust:1.78-bookworm AS builder

# System dependencies for rdkafka (cmake build) and tonic (protobuf)
RUN apt-get update && apt-get install -y \
    protobuf-compiler libprotobuf-dev cmake libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./
COPY proto/ ./proto/

# Copy each service directory for dependency caching
COPY gateway/Cargo.toml  gateway/Cargo.toml
COPY kms/Cargo.toml      kms/Cargo.toml
COPY sessions/Cargo.toml sessions/Cargo.toml
COPY indexer/Cargo.toml  indexer/Cargo.toml
COPY engine/Cargo.toml   engine/Cargo.toml
COPY ws-metrics/Cargo.toml ws-metrics/Cargo.toml

# Create stub src/main.rs for all services to cache dependencies
RUN for svc in gateway kms sessions indexer engine ws-metrics; do \
    mkdir -p $svc/src && echo 'fn main(){}' > $svc/src/main.rs; \
    done

# Warm dependency cache (will fail gracefully for missing build.rs)
RUN cargo build --release 2>/dev/null || true

# Copy actual source + build scripts
COPY gateway/   ./gateway/
COPY kms/       ./kms/
COPY sessions/  ./sessions/
COPY indexer/   ./indexer/
COPY engine/    ./engine/
COPY ws-metrics/ ./ws-metrics/

# Build the specific service
RUN cargo build --release -p engine

# ── Runtime stage ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/engine /usr/local/bin/app

EXPOSE 8080
CMD ["/usr/local/bin/app"]
