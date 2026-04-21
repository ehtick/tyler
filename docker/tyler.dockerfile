FROM rust:1-bookworm AS builder

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
COPY --from=cjlib . /usr/src/cjlib
COPY --from=cjindex . /usr/src/cjindex
COPY --from=cityjson-rs . /usr/src/cityjson-rs
COPY --from=cityarrow . /usr/src/cityarrow
COPY --from=serde_cityjson . /usr/src/serde_cityjson
RUN --mount=type=cache,target=/usr/src/tyler/target cargo install --path .

COPY docker/strip-docker-image-export ./
RUN rm -rf /export
RUN mkdir /export && \
    bash ./strip-docker-image-export \
    -v \
    -d /export \
    -f /usr/local/share/proj/proj.db \
    -f /usr/local/cargo/bin/tyler

FROM ubuntu:lunar-20230301
ARG VERSION
LABEL org.opencontainers.image.authors="Balázs Dukai <balazs.dukai@3dgi.nl>"
LABEL org.opencontainers.image.vendor="3DGI"
LABEL org.opencontainers.image.title="tyler"
LABEL org.opencontainers.image.description="Create tiles from 3D city objects encoded as CityJSONFeatures."
LABEL org.opencontainers.image.version=$VERSION
LABEL org.opencontainers.image.license="(APACHE-2.0 AND GPL-3 AND AGPL-3)"

RUN rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/share/proj /usr/local/share/proj
COPY --from=builder /export/lib/ /lib/
COPY --from=builder /export/lib64/ /lib64/
COPY --from=builder /export/usr/ /usr/
COPY --from=builder /export/usr/local/cargo/bin/tyler /usr/local/bin/tyler

# Update library links
RUN ldconfig
CMD ["tyler"]
