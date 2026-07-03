FROM rust:1-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        clang \
        cmake \
        gdal-bin \
        libclang-dev \
        libgdal-dev \
        libgeos-dev \
        libproj-dev \
        pkg-config \
        postgresql-client \
        sqlite3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
