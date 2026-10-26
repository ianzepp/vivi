# vivi-server: Rust build, slim runtime.
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/vivi-server /usr/local/bin/vivi-server
# Env plumbing; STORE_ENCRYPTION_KEY and MODERATION_TOKEN come from the
# platform secret store (Railway variables), never baked into the image.
# NOTE: no Docker VOLUME directive — Railway rejects it; the /data volume is
# attached on the platform (railway add --volume --mount-path /data).
ENV PORT=8080 \
    DATA_DIR=/data
EXPOSE 8080
CMD ["vivi-server"]
