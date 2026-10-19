FROM rust:1.90-bookworm AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/menderctl /usr/bin/menderctl
ENTRYPOINT ["/usr/bin/menderctl"]
