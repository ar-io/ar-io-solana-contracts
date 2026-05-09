# AR.IO Solana contracts builder image.
#
# Based on solanafoundation/anchor:v0.31.1's Dockerfile with two changes:
#   1. Rust toolchain pinned (not latest-stable) so clippy/fmt lints are
#      reproducible across local dev and CI.
#   2. rustfmt + clippy components installed explicitly.
#
# Build:
#   docker build --build-arg RUST_TOOLCHAIN=1.90.0 \
#                --build-arg ANCHOR_CLI=v0.31.1    \
#                --build-arg SOLANA_CLI=v2.1.0     \
#                -t ghcr.io/ar-io/anchor-builder:latest docker/
#
# Push (requires `docker login ghcr.io`):
#   docker push ghcr.io/ar-io/anchor-builder:latest

FROM ubuntu:22.04

ARG DEBIAN_FRONTEND=noninteractive

ARG RUST_TOOLCHAIN=1.90.0
ARG SOLANA_CLI=v2.1.0
ARG ANCHOR_CLI=v0.31.1
ARG NODE_VERSION=v20.18.0

ENV HOME="/root"
ENV PATH="${HOME}/.cargo/bin:${PATH}"
ENV PATH="${HOME}/.local/share/solana/install/active_release/bin:${PATH}"
ENV PATH="${HOME}/.nvm/versions/node/${NODE_VERSION}/bin:${PATH}"

RUN mkdir -p /workdir && mkdir -p /tmp && \
    apt-get update -qq && apt-get upgrade -qq && apt-get install -qq \
    build-essential git curl wget jq pkg-config python3-pip \
    libssl-dev libudev-dev clang mold

# Rust — pinned to $RUST_TOOLCHAIN for reproducible lints.
RUN curl "https://sh.rustup.rs" -sfo rustup.sh && \
    sh rustup.sh -y --default-toolchain ${RUST_TOOLCHAIN} && \
    rustup component add rustfmt clippy

# Node / npm / yarn (used by IDL snapshot checks).
RUN curl -o- https://raw.githubusercontent.com/creationix/nvm/v0.33.11/install.sh | bash
ENV NVM_DIR="${HOME}/.nvm"
RUN . $NVM_DIR/nvm.sh && \
    nvm install ${NODE_VERSION} && \
    nvm use ${NODE_VERSION} && \
    nvm alias default node && \
    npm install -g yarn

# Solana CLI.
RUN sh -c "$(curl -sSfL https://release.anza.xyz/${SOLANA_CLI}/install)"

# Anchor CLI — built with the pinned host toolchain.
RUN cargo install --git https://github.com/coral-xyz/anchor --tag ${ANCHOR_CLI} anchor-cli --locked

# cargo-nextest — faster test runner with per-test process isolation.
# Pre-built binary avoids a 5+ minute compile from source.
RUN curl -LsSf https://get.nexte.st/0.9/linux | tar zxf - -C /root/.cargo/bin

# NOTE: we intentionally skip the dummy-project BPF bootstrap that
# upstream solanafoundation/anchor does.  The dummy's unpinned
# Cargo.lock pulls HEAD crate versions that may be incompatible with
# BPF Cargo 1.79 (e.g. cpufeatures 0.3 uses edition2024).  Real
# builds bring their own pinned Cargo.lock; the BPF platform-tools
# download (~30s) happens on the first `anchor build` in CI.

# Use mold linker for ~3-5x faster linking of host-target test binaries.
# Only affects host builds; BPF builds (cargo-build-sbf) use their own linker.
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="clang"
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"

WORKDIR /workdir
