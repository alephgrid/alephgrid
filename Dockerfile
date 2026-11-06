# syntax=docker/dockerfile:1

FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web/ ./
COPY crates/alephgrid-core/data/ /src/crates/alephgrid-core/data/
RUN npm run build

FROM rust:1.97-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock* rust-toolchain.toml ./
COPY crates ./crates
COPY vendor ./vendor
COPY --from=web /src/web/dist ./web/dist
RUN cargo build --release --locked -p alephgrid-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 alephgrid
COPY --from=server /src/target/release/alephgrid /usr/local/bin/alephgrid
COPY LICENSE NOTICE THIRD-PARTY-NOTICES.md /usr/share/doc/alephgrid/
USER alephgrid
WORKDIR /home/alephgrid
ENV ALEPHGRID_HOST=0.0.0.0 \
    ALEPHGRID_PORT=7878 \
    ALEPHGRID_AUTH=accounts \
    ALEPHGRID_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 7878
ENTRYPOINT ["alephgrid"]
