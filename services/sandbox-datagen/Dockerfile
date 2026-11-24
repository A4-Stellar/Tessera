# Build stage — the datagen crate has zero external dependencies.
FROM rust:1.81-alpine AS builder
WORKDIR /build
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release --offline

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=builder /build/target/release/tessera-sandbox-datagen /usr/local/bin/tessera-sandbox-datagen
ENTRYPOINT ["tessera-sandbox-datagen"]
