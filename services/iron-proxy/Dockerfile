# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.46.0-rc.1@sha256:8d78eb486e4298078fe0fa19803094dd6cf1b8ca12d956ae5e1cdfcac0f5e5c2

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
