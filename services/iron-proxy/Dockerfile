# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.43.0@sha256:90c8e04c4509bf2954fe6bf0038922740ecf8d133a8ae016fbf880c453e6ef47

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
