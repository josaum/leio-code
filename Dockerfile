FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
COPY crates ./crates
# Build dependencies - this is the caching Docker layer!
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release -p leio-code -p leio-harness && \
    cp /app/target/release/leio-code /usr/local/bin/leio-code && \
    cp /app/target/release/leio-harness /usr/local/bin/leio-harness

# We do not need the Rust toolchain to run the binary!
FROM debian:trixie-slim AS runtime
WORKDIR /app
COPY --from=builder /usr/local/bin/leio-code /usr/local/bin/leio-code
COPY --from=builder /usr/local/bin/leio-harness /usr/local/bin/leio-harness
# FCA concept-lattice induction runs in the pre-built fca_fast parser wheel
# via uv; without python3/uv the wheel-backed paths degrade gracefully.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && curl -LsSf https://astral.sh/uv/install.sh | sh \
    && mv /root/.local/bin/uv /usr/local/bin/uv && mv /root/.local/bin/uvx /usr/local/bin/uvx || true
COPY artifacts ./artifacts
ENV LEIO_FCA_FIND_LINKS=/app/artifacts/wheels/manylinux
ENTRYPOINT ["/usr/local/bin/leio-code"]
