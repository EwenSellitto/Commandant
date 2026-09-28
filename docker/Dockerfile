# syntax=docker/dockerfile:1

FROM rust:1-slim-trixie AS builder
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p commandant-cli \
    && cp target/release/commandant /usr/local/bin/commandant

FROM debian:trixie-slim
# git and ca-certificates are what coding agents need on a worker; curl
# lets it install a harness (--harness).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/commandant /usr/local/bin/commandant

ENV COMMANDANT_STATE_DIR=/state \
    COMMANDANT_DATA_DIR=/data
WORKDIR /work
EXPOSE 7400

ENTRYPOINT ["commandant"]
CMD ["worker"]
