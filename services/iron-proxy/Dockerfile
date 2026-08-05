# syntax=docker/dockerfile:1.7

ARG IRON_PROXY_BASE_IMAGE=ironsh/iron-proxy:0.50.0-rc.2@sha256:afff3a56cfe0cea8f2c93f2bae1e650722740ea3b626110448d7bb47e642be76
FROM ${IRON_PROXY_BASE_IMAGE}

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
