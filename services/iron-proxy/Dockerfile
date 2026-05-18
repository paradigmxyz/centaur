FROM ironsh/iron-proxy:0.35.0@sha256:a2b341c26c4e1ee2e56abfbe4799f749ab71695e91e3ea3260732712c5ed1bd0

USER root
RUN apk add --no-cache curl jq

COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
