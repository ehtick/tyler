FROM rust:1-bookworm AS base

USER root

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    clang-15 \
    cmake \
    libsqlite3-dev \
    pkg-config \
    proj-data \
    unzip \
    wget && \
    rm -rf /var/lib/apt/lists/*

# Download Dutch transformation grids
RUN mkdir -p /usr/local/share/proj && \
    cp -a /usr/share/proj/. /usr/local/share/proj/ && \
    wget https://cdn.proj.org/nl_nsgi_nlgeo2018.tif -O /usr/local/share/proj/nl_nsgi_nlgeo2018.tif && \
    wget https://cdn.proj.org/nl_nsgi_rdtrans2018.tif -O /usr/local/share/proj/nl_nsgi_rdtrans2018.tif

WORKDIR /usr/src/tyler

COPY Cargo.toml Cargo.lock ./
COPY cityjson-convert ./cityjson-convert
COPY resources ./resources
COPY src ./src
COPY proj ./proj

RUN --mount=type=cache,target=/usr/src/tyler/target-docker CARGO_TARGET_DIR=/usr/src/tyler/target-docker cargo build --manifest-path ./Cargo.toml
