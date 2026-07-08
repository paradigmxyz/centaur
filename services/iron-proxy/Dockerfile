# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.46.0@sha256:ce65e5efe68635b0867005d7b7b730f58b5aa112433b418a28d254682706a2fb

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
