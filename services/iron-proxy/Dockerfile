# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.60.0@sha256:7718f9cc63f1b60fa462f41633cdd3a623d26a3d3d72e7014d5e268e172f5407

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY services/iron-proxy/iron-proxy-tenancy.yaml /usr/local/share/iron-proxy/tenancy.yaml
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
