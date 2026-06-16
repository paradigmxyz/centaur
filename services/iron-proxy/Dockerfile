# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.44.0@sha256:1038c4559d60770f46199ee2ed7c61efcf62cac604a4f3436c3d9f0ec8067b29

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
