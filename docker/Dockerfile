# Build context: the repository root (see docker-compose.yml).
#
#   docker build -f docker/Dockerfile -t pixi-fleet-agent .
#
# NOTE: this image has not been built in the environment this project was
# written in (no Docker daemon was available there). The pieces it uses (cargo
# build, the pixi installer, the agent CLI) were each run separately.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY agent/Cargo.toml agent/Cargo.lock ./agent/
COPY agent/src ./agent/src
COPY agent/ui ./agent/ui
RUN cd agent && cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
# Pixi, installed system-wide (the agent shells out to it)
RUN curl -fsSL https://pixi.sh/install.sh | PIXI_BIN_DIR=/usr/local/bin PIXI_NO_PATH_UPDATE=1 bash
COPY --from=build /src/agent/target/release/agent /usr/local/bin/agent
ENV PIXI_HOME=/data/.pixi-home
VOLUME /data
EXPOSE 8000
ENTRYPOINT ["agent"]
CMD ["run", "--id", "robot", "--listen", "0.0.0.0:8000", "--data-dir", "/data"]
