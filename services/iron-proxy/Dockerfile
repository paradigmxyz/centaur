# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.52.0@sha256:004fdc067dc4cc47cebfc0dab3c63ab9a5e21a3230c965e69775a565df99efc6

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
