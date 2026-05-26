FROM ironsh/iron-proxy:0.42.0-rc.2@sha256:b8ee863804c90369344db9399f3deaed15173b952c1bc631070681b70428fae5

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
