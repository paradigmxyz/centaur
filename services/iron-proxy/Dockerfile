# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.55.0@sha256:a77e4aed1b3dd5e4d70198a89c6a489b6361041f9fa4ed3aa8df31e3c91dd286

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
