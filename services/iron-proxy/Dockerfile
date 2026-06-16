# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.44.0@sha256:ac4159f987e781a5c6011a880e338794155171884de005e4c9c7b5329786bbe5

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
