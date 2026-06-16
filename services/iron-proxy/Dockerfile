# syntax=docker/dockerfile:1.7

# Pin the multi-arch image index digest (not a single-arch manifest) so the
# amd64 and arm64 build matrix each resolve to the matching platform. Pinning
# an arch-specific digest here builds an amd64 base on arm64 runners, which
# fails with "exec format error".
FROM ironsh/iron-proxy:0.44.0@sha256:1038c4559d60770f46199ee2ed7c61efcf62cac604a4f3436c3d9f0ec8067b29

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
