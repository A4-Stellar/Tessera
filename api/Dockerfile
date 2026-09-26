# syntax=docker/dockerfile:1
#
# Production image for the Tessera API (#26): a fully static musl binary on a
# distroless base — no shell, no package manager, no libc to patch.
#
#   Single arch (host):  docker build -t tessera-api api/
#   Multi-arch:          docker buildx build --platform linux/amd64,linux/arm64 -t tessera-api api/
#
# The builder always runs on the build host's native platform
# ($BUILDPLATFORM) and cross-compiles with cargo-zigbuild: Zig supplies the C
# compiler and linker for the crates with native code (ring, aws-lc-sys), so
# arm64 images are built without slow QEMU emulation.

# 1.88 is the highest rust-version required by the locked dependencies.
ARG RUST_VERSION=1.88

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3-pip \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --no-cache-dir --break-system-packages cargo-zigbuild==0.23.4 ziglang==0.16.0 \
    && cargo install --locked cargo-chef --version 0.1.78 \
    && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
WORKDIR /app

# Dependency recipe: rebuilt only when manifests or sources change.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG TARGETARCH
RUN case "$TARGETARCH" in \
        amd64) echo x86_64-unknown-linux-musl ;; \
        arm64) echo aarch64-unknown-linux-musl ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac > /rust-target
# musl targets link the C runtime statically; make it explicit for both
# architectures so the binary never depends on a loader or libc.
ENV RUSTFLAGS="-C target-feature=+crt-static"

# Cache dependencies separately from source.
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --zigbuild --target "$(cat /rust-target)" --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Root-owned and read/execute-only, so a compromised process cannot
# overwrite its own executable.
RUN cargo zigbuild --release --locked --target "$(cat /rust-target)" \
    && install -D -m 0555 "target/$(cat /rust-target)/release/tessera-api" /out/tessera-api

# Static base: CA certificates, tzdata and /etc/passwd only.
FROM gcr.io/distroless/static-debian12:nonroot AS runtime
COPY --from=builder /out/tessera-api /usr/local/bin/tessera-api

ENV PORT=8080
EXPOSE 8080
# The image's `nonroot` user, as numeric UID:GID so orchestrators that enforce
# runAsNonRoot/runAsUser by UID (e.g. Kubernetes PodSecurity) can verify it
# without resolving /etc/passwd.
USER 65532:65532
# No curl or shell in a distroless image: the binary probes its own /health.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/tessera-api", "healthcheck"]
CMD ["tessera-api"]
