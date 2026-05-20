FROM ironsh/iron-proxy:0.38.0@sha256:724c075e22e962e3452a95cd0680a195a42082033bb830b1eed0d4b7911b5626

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
