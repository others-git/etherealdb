# ---- build stage: static musl binary ----
FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
# Survive flaky crates.io downloads during the build.
ENV CARGO_NET_RETRY=10 CARGO_NET_GIT_FETCH_WITH_CLI=true

# Fetch + compile dependencies first, against a dummy crate, so they cache in a
# layer that source edits don't invalidate. cargo fetch retries on flaky pulls.
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --offline --bin etherealdb 2>/dev/null || true \
    && rm -rf src

COPY . .
# Touch so cargo rebuilds with the real sources (deps already cached, offline).
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --offline --bin etherealdb

# ---- runtime stage: tiny Alpine image ----
FROM alpine:3.20
LABEL org.opencontainers.image.title="EtherealDB" \
      org.opencontainers.image.description="A database that isn't there: speaks real wire protocols, returns plausible nonsense." \
      org.opencontainers.image.source="https://github.com/others-git/etherealdb"

RUN adduser -D -u 10001 ethereal
COPY --from=builder /app/target/release/etherealdb /usr/local/bin/etherealdb

USER ethereal
EXPOSE 5432 3306
ENTRYPOINT ["etherealdb"]
# Listen on all interfaces so the container is reachable; speak both protocols.
CMD ["--pg", "0.0.0.0:5432", "--mysql", "0.0.0.0:3306"]
