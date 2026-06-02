FROM ironsh/iron-proxy:0.42.0-rc.6@sha256:f94f2afd336415a71749cacafee2bf77ff0c23edbfc4f22b07f0017b58a2b9d0

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
