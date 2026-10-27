# vivi server image: Rust build, slim runtime. UNUSED leftover (Railway retired
# 2026-10-21); kept so the configuration shape is on record.
#
# The server reads no per-setting environment variables. VIVI_HOME=/data is the
# only one: the server home is /data/server, and the image expects these on the
# mounted volume (nothing is baked into the image):
#   /data/server/config.toml   optional keys: bind, port, data_dir, ...
#                              (bind must be "0.0.0.0" for a container to be reachable)
#   /data/server/creds/        mode 700; STORE_ENCRYPTION_KEY, MODERATION_TOKEN,
#                              HOST_OPERATOR_TOKEN as files, mode 600
# `vivi server init --port N` (run once with VIVI_HOME=/data) creates all of it;
# see the Server section of skills/vivi/SKILL.md.
# NOTE: no Docker VOLUME directive (Railway rejects it); the /data volume is
# attached on the platform (railway add --volume --mount-path /data).
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/vivi /usr/local/bin/vivi
ENV VIVI_HOME=/data
EXPOSE 8080
CMD ["vivi", "server", "run"]
