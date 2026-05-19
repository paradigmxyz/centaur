FROM ironsh/iron-proxy:0.36.0@sha256:597856e32cc2f3256472922cd3610ad5415ec6ba91409501f0a64f5f0bc4fce9

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
