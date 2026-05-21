FROM ironsh/iron-proxy:0.39.0@sha256:a2e124f1266823f3d21ada5e3bd5611d85cdbb8ca8866047ff2b18ed122a274b

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
