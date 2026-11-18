# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.57.0@sha256:3b7ace7bc766ef02376453fa60aefc40ab2f75c4e3fd73c16d64b274893d854e

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
