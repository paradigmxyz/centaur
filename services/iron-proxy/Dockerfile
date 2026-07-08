# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.47.0@sha256:dc13ff78c9e2c83389dc540c8f51cf551edc543900855200a49ccf7ca6fd9666

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
