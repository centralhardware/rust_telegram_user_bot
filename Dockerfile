FROM rust:1-slim AS chef

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
# Installed in its own layer so it is cached independently of the sources.
RUN cargo install cargo-chef --locked

WORKDIR /app

# Reduce the manifests to a recipe that only changes when the dependencies do.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# Build the dependencies alone. This layer is reused for every source-only
# change, which is what keeps the image build off the full grammers rebuild.
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release && ls -la /app/target/release/telegram*

FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/telegram_user_bot /app/telegram_user_bot

USER 10001

ENTRYPOINT ["/app/telegram_user_bot"]
