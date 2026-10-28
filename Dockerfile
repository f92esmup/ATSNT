# syntax=docker/dockerfile:1.7
# =============================================================================
# ATSNT - Batch image for the ephemeral HPO pipeline (fetch -> HPO -> backtest).
#
# Market data is written inside the container filesystem (/app/work), which lives
# on the VM boot disk and disappears with the VM: nothing is persisted to GCS.
#
# Build:  docker build --build-arg GIT_SHA=$(git rev-parse --short=12 HEAD) -t atsnt-batch .
# Run:    docker run --rm atsnt-batch --year 2026 --month 1 --skip-fetch ...
#         (arguments are passed to scripts/run_production_pipeline.sh)
# =============================================================================

ARG RUST_VERSION=1.99
ARG DEBIAN_SUITE=bookworm

FROM rust:slim-bookworm AS builder
WORKDIR /src

# .git is not in the build context; build.rs falls back to this value for run metadata.
ARG GIT_SHA=""
ENV ATSNT_GIT_SHA=${GIT_SHA}

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
        -p adapters --bin fetch_data \
        -p backtest --bin run_hpo \
        -p backtest --bin backtest \
        -p backtest --bin calibrate_feeds \
        -p backtest --bin prune_swarm \
        -p backtest --bin run_portfolio \
        -p backtest --bin run_portfolio_hpo \
    && mkdir -p /out \
    && cp target/release/fetch_data target/release/run_hpo target/release/backtest target/release/calibrate_feeds target/release/prune_swarm target/release/run_portfolio target/release/run_portfolio_hpo /out/

FROM debian:${DEBIAN_SUITE}-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 --shell /usr/sbin/nologin atsnt

WORKDIR /app
COPY --from=builder /out/ /app/bin/
COPY scripts/run_production_pipeline.sh /app/scripts/run_production_pipeline.sh
COPY scripts/run_joint_portfolio_pipeline.sh /app/scripts/run_joint_portfolio_pipeline.sh
COPY configs/ /app/configs/

RUN chmod +x /app/scripts/*.sh

# Writable work area for downloaded market data, logs and the winning artifact.
RUN mkdir -p /app/work && chown -R atsnt:atsnt /app/work /app/configs

USER atsnt

# Defaults point the pipeline at the prebuilt binaries and the writable work area;
# later arguments (e.g. --output-dir) override them.
ENTRYPOINT ["/app/scripts/run_production_pipeline.sh", "--bin-dir", "/app/bin", "--no-inhibit", "--output-dir", "/app/work/data"]
