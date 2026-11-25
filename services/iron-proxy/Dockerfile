# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.61.0@sha256:973b7adf44a12ded3b58cf67af35a80a8304d82bef0fb943e6467fe4f4020573

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY services/iron-proxy/iron-proxy-tenancy.yaml /usr/local/share/iron-proxy/tenancy.yaml
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
