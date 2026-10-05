# The published image (ghcr.io/dissssy/synththing): a release's Linux
# binary (synththing-linux-x86_64, next to this file when it's built: see
# .github/workflows/release.yml) in the same runtime as ../Dockerfile.

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libasound2 libudev1 libgtk-3-0 libxkbcommon0 libwayland-client0 \
    && rm -rf /var/lib/apt/lists/*
COPY synththing-linux-x86_64 /usr/local/bin/synththing
ENV SYNTHTHING_DOCKER=1
VOLUME /data
EXPOSE 7381
ENTRYPOINT ["synththing"]
CMD ["serve", "--data", "/data", "--bind", "0.0.0.0:7381"]
