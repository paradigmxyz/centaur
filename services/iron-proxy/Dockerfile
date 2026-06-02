FROM ironsh/iron-proxy:0.42.0-rc.5@sha256:e607a41ed276dc41fcc976fe840505c884bf9496da9ac0caa0e6361357cfae83

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
