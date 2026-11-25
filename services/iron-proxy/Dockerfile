# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.59.0@sha256:30824bb4b8ba0703bd1acfe57e6ba2fb8dde041387df7a1fc776a807e3f111f5

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
