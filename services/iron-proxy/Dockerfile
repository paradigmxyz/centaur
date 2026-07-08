# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.48.0@sha256:1b5de5556a5fa9855d33d5755a2d2b48cc4c7a5e2928e5c0d8cec67c80c247fb

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
