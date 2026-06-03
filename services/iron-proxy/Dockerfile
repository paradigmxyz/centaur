FROM ironsh/iron-proxy:0.42.0-rc.7@sha256:162a95ba9326580d6a8fdce3a505f7c29e17673cd3e8d45bd4852d9c0df2615c

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
