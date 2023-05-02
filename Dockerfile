FROM rust:1-bullseye as builder
WORKDIR /home/echo-server

RUN apt-get update \
    && apt-get install -y protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

COPY ./server/src/ ./server/src/
COPY ./server/Cargo.toml ./server/Cargo.toml
COPY ./server/Cargo.lock ./server/Cargo.lock
WORKDIR /home/echo-server/server
RUN cargo build --locked --release

FROM debian:bullseye-slim
LABEL org.opencontainers.image.source=https://github.com/francoposa/echo-server-rust-logging-metrics-tracing/echo-server

COPY --from=builder /home/echo-server/server/target/release/echo-server-logging-metrics-tracing /usr/local/bin/echo-server
CMD ["echo-server"]