# syntax=docker/dockerfile:1.7

FROM public.ecr.aws/docker/library/golang:1.26.5-alpine3.24@sha256:0178a641fbb4858c5f1b48e34bdaabe0350a330a1b1149aabd498d0699ff5fb2 AS route-selector-builder
ARG IRON_PROXY_ROUTE_SELECTOR_SOURCE=https://codeload.github.com/PengDeng-Cyber/iron-proxy/tar.gz/121724190bdd3f6b1a551f311d2e8b5c23a0dce0
ARG IRON_PROXY_ROUTE_SELECTOR_SHA256=c50e2e9121c0980c24d89e8d2bf48fdf02c28a2b22af595bb598f792f05423f1
RUN apk add --no-cache ca-certificates curl tar
WORKDIR /src
RUN curl --fail --location --silent --show-error "$IRON_PROXY_ROUTE_SELECTOR_SOURCE" --output source.tar.gz \
    && echo "$IRON_PROXY_ROUTE_SELECTOR_SHA256  source.tar.gz" | sha256sum -c - \
    && tar --extract --gzip --file source.tar.gz --strip-components=1 \
    && CGO_ENABLED=0 go build -trimpath \
        -ldflags="-s -w -X github.com/ironsh/iron-proxy/internal/version.Version=0.49.0-centaur-route-selector.1" \
        -o /out/iron-proxy ./cmd/iron-proxy

FROM ironsh/iron-proxy:0.49.0@sha256:c4628019c24f4cc8d77564a26b7c9cedb00accee6f93d06270e85fb8f9c6a7da

USER root
RUN --mount=type=cache,target=/var/cache/apk,sharing=locked \
    apk add --update-cache --cache-dir /var/cache/apk curl jq

COPY --from=route-selector-builder /out/iron-proxy /usr/local/bin/iron-proxy
COPY services/iron-proxy/iron-proxy.yaml /usr/local/share/iron-proxy/proxy.yaml.default
COPY --chmod=755 services/iron-proxy/entrypoint.sh /entrypoint.sh

EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
