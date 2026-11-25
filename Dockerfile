# syntax=docker/dockerfile:1

# Веб-интерфейс собирается отдельно, в образ попадает только web/dist.
FROM node:24-slim AS web
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.99-slim-bookworm AS rust
ARG AGENT_VERSION
# reqwest использует native-tls.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    AGENT_VERSION="$AGENT_VERSION" cargo build --release --locked -p cli \
    && cp target/release/rs /usr/local/bin/rs

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home app
WORKDIR /app
COPY --from=rust /usr/local/bin/rs /usr/local/bin/rs
COPY --from=web /app/web/dist web/dist
COPY samples samples
COPY eval eval
RUN mkdir -p .cache reports && chown app:app .cache reports
USER app
ENV RUST_LOG=warn
EXPOSE 3333
ENTRYPOINT ["rs"]
CMD ["serve", "--listen", "0.0.0.0:3333", "--provider", "mock"]
