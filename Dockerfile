# A synththing script library server (docs/DEPLOY.md), built from source.
# deploy/docker-compose.build.yml uses this; the published image
# (ghcr.io/dissssy/synththing) is built from release binaries instead
# (deploy/release.Dockerfile), with the same runtime.
#
# It's the same program as the app, so it links the desktop libraries
# (GTK, ALSA, udev) even though `serve` never opens a window.

FROM rust:1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends libasound2-dev libudev-dev libgtk-3-dev libxkbcommon-dev libwayland-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --bin synththing

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libasound2 libudev1 libgtk-3-0 libxkbcommon0 libwayland-client0 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/synththing /usr/local/bin/synththing
# Tells the server it runs in a container (so it says to update by
# pulling a new image, not in place).
ENV SYNTHTHING_DOCKER=1
VOLUME /data
EXPOSE 7381
ENTRYPOINT ["synththing"]
CMD ["serve", "--data", "/data", "--bind", "0.0.0.0:7381"]
