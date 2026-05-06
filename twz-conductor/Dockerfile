ARG RUST_TOOLCHAIN=1.85
FROM rust:${RUST_TOOLCHAIN} AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./

COPY twz-conductor/Cargo.toml twz-conductor/Cargo.toml
COPY twz-gateway/Cargo.toml twz-gateway/Cargo.toml
COPY twz-cellserver/Cargo.toml twz-cellserver/Cargo.toml
COPY twz-shared/Cargo.toml twz-shared/Cargo.toml

RUN mkdir -p twz-conductor/src twz-gateway/src twz-cellserver/src twz-shared/src && \
    echo "fn main() {}" > twz-conductor/src/main.rs && \
    echo "fn main() {}" > twz-gateway/src/main.rs && \
    echo "fn main() {}" > twz-cellserver/src/main.rs && \
    echo "" > twz-shared/src/lib.rs
RUN cargo fetch


COPY . .

RUN cargo build --release --bin twz-conductor


FROM gcr.io/distroless/cc-debian12
WORKDIR /app

COPY --from=builder /app/target/release/twz-conductor /usr/local/bin/app
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/app"]