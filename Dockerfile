# syntax=docker/dockerfile:1
FROM rust:1-alpine AS builder
WORKDIR /app
ARG CARGO_BUILD_JOBS=2
ARG TARGETARCH
RUN apk add --no-cache build-base pkgconf ca-certificates
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY workflows ./workflows
RUN --mount=type=cache,id=docker-sync-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=docker-sync-target-${TARGETARCH},target=/app/target,sharing=locked \
    cargo build --locked --release --jobs $CARGO_BUILD_JOBS --no-default-features --features web --bin docker-sync-web \
    && cp target/release/docker-sync-web /docker-sync-web
RUN mkdir -p /runtime/app /runtime/data /runtime/tmp \
    && chmod 1777 /runtime/tmp && chown 10001:10001 /runtime/data

FROM scratch
COPY --from=builder /docker-sync-web /app/docker-sync-web
COPY --from=builder --chown=10001:10001 /runtime/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY templates /app/templates
COPY static /app/static
ENV DATA_DIR=/data ASSET_DIR=/app ROCKET_ADDRESS=0.0.0.0 ROCKET_PORT=8000 SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
USER 10001:10001
WORKDIR /app
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["/app/docker-sync-web", "health"]
ENTRYPOINT ["/app/docker-sync-web"]
