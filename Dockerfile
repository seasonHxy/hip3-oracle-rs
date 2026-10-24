FROM rust:1.96.0-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 oracle && useradd --uid 10001 --gid 10001 --no-create-home oracle \
    && mkdir -p /app/state && chown 10001:10001 /app/state
COPY --from=build /build/target/release/hip3-oracle-rs /usr/local/bin/hip3-oracle-rs
COPY LICENSE /usr/share/doc/hip3-oracle-rs/LICENSE
COPY config/example.json config/container.example.json /app/config/
WORKDIR /app
USER 10001:10001
STOPSIGNAL SIGTERM
ENTRYPOINT ["hip3-oracle-rs"]
CMD ["--config", "/app/config/container.example.json", "run"]
