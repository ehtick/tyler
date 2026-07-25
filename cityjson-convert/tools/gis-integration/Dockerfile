FROM rust:1-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        cmake \
        gdal-bin \
        libgdal-dev \
        libproj-dev \
        pkg-config \
        sqlite3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
