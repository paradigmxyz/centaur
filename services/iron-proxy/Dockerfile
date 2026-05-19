FROM ironsh/iron-proxy:0.37.0@sha256:f480a0f8233b8bb180e5837e24feeb71f214eaa10fdfa2439dd9aed7ecee62d6

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
