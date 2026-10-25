# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci || npm install
COPY web/ ./
RUN npm run build

FROM rust:1.97-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock* rust-toolchain.toml ./
COPY crates ./crates
COPY --from=web /src/web/dist ./web/dist
RUN cargo build --release --locked -p rowfinity-server || cargo build --release -p rowfinity-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 rowfinity
COPY --from=server /src/target/release/rowfinity /usr/local/bin/rowfinity
USER rowfinity
WORKDIR /home/rowfinity
ENV ROWFINITY_HOST=0.0.0.0 \
    ROWFINITY_PORT=7878 \
    ROWFINITY_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 7878
ENTRYPOINT ["rowfinity"]
