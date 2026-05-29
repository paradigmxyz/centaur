FROM ironsh/iron-proxy:0.42.0-rc.3@sha256:d62792cbc1121e0277cb6bf76df53bf6d1050748cbe625a9ac3039dff1cee88f

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
