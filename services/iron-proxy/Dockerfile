# syntax=docker/dockerfile:1.7

FROM ironsh/iron-proxy:0.42.0-rc.8@sha256:0fb21317bdec25321e7215e6538357164ec2df6840649cdbe5ca6922d22ba79e

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
