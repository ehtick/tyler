FROM rust:1.97.1-trixie AS builder

USER root

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    cmake \
    libsqlite3-dev \
    pkg-config \
    libproj-dev \
    proj-data \
    unzip \
    wget \
    sqlite3 && \
    rm -rf /var/lib/apt/lists/*

# Download Dutch transformation grids
RUN mkdir -p /usr/local/share/proj && \
    cp -a /usr/share/proj/. /usr/local/share/proj/ && \
    wget https://cdn.proj.org/nl_nsgi_nlgeo2018.tif -O /usr/local/share/proj/nl_nsgi_nlgeo2018.tif && \
    wget https://cdn.proj.org/nl_nsgi_rdtrans2018.tif -O /usr/local/share/proj/nl_nsgi_rdtrans2018.tif

WORKDIR /usr/src/tyler
COPY Cargo.toml Cargo.lock ./
COPY cityjson-convert ./cityjson-convert
COPY src ./src

RUN --mount=type=cache,target=/usr/src/tyler/target cargo install --features proj-system --path .

COPY docker/strip-docker-image-export ./
RUN rm -rf /export
RUN mkdir /export && \
    bash ./strip-docker-image-export \
    -v \
    -d /export \
    -f /usr/local/share/proj/proj.db \
    -f /usr/local/cargo/bin/tyler

FROM debian:trixie-slim
ARG VERSION
LABEL org.opencontainers.image.authors="Balázs Dukai <balazs.dukai@3dgi.nl>"
LABEL org.opencontainers.image.vendor="3DGI"
LABEL org.opencontainers.image.title="tyler"
LABEL org.opencontainers.image.description="Create tiles from 3D city objects encoded as CityJSON."
LABEL org.opencontainers.image.version=$VERSION
LABEL org.opencontainers.image.license="(APACHE-2.0 AND GPL-3 AND AGPL-3)"

RUN apt-get update && apt-get install -y --no-install-recommends \
    libc6 \
    libproj-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/share/proj /usr/local/share/proj
COPY --from=builder /export/usr/local/cargo/bin/tyler /usr/local/bin/tyler

# Update library links
RUN ldconfig
CMD ["tyler"]
