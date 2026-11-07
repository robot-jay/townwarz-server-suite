# Build stage
ARG RUST_TOOLCHAIN=1.85
FROM rust:${RUST_TOOLCHAIN} AS builder
WORKDIR /app
# Faster builds: cache deps
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
# Copy workspace manifests so dependency resolution works
COPY twz-conductor/Cargo.toml twz-conductor/Cargo.toml
COPY twz-gateway/Cargo.toml twz-gateway/Cargo.toml
COPY twz-cellserver/Cargo.toml twz-cellserver/Cargo.toml
COPY twz-shared/Cargo.toml twz-shared/Cargo.toml
COPY twz-schema/Cargo.toml twz-schema/Cargo.toml
COPY twz-migrate/Cargo.toml twz-migrate/Cargo.toml
# Create empty src for each to let cargo fetch deps
RUN mkdir -p twz-conductor/src twz-gateway/src twz-cellserver/src twz-shared/src twz-schema/src twz-migrate/src && \
    echo "fn main() {}" > twz-conductor/src/main.rs && \
    echo "fn main() {}" > twz-gateway/src/main.rs && \
    echo "fn main() {}" > twz-cellserver/src/main.rs && \
    echo "" > twz-shared/src/lib.rs && \
    echo "" > twz-schema/src/lib.rs && \
    echo "" > twz-migrate/src/lib.rs && \
    echo "fn main() {}" > twz-migrate/src/main.rs
RUN cargo fetch --locked

# Now copy sources and build the real binary
COPY . .

RUN cargo build --release --locked --bin twz-gateway

# Runtime stage
FROM gcr.io/distroless/cc-debian12
WORKDIR /app
# Change the binary name per crate
COPY --from=builder /app/target/release/twz-gateway /usr/local/bin/app
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/app"]
