# A remote development environment as the e2e stack's SSH scenarios reach it:
# a wstunnel v11 WSS gateway in front of OpenSSH, the deployment shape
# iron-proxy's SSH listener dials (docs/pages/operate/ssh.mdx). e2e/stack.sh
# builds this image and loads it into kind; e2e/infra/ssh-gateway.yaml runs it.
FROM ghcr.io/erebe/wstunnel:v11.1.0@sha256:e2b62fac898406c73342e8e50fc21f7e221c17811aad532ed3a80a2e3c327b79 AS wstunnel

FROM debian:trixie-slim

# The package generates the fixture host keys; iron-proxy trusts the gateway's
# TLS identity instead.
RUN apt-get update \
    && apt-get install -y --no-install-recommends openssh-server procps \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -m 0755 /run/sshd \
    # A '*' password leaves the account unlocked for key logins without PAM.
    && useradd -m -s /bin/sh dev && usermod -p '*' dev \
    && install -d -o dev -g dev -m 0755 /workspace \
    && install -d -o 1000 -g 1000 /home/app

COPY --from=wstunnel /home/app/wstunnel /usr/local/bin/wstunnel
COPY --chmod=0644 sshd_config /etc/ssh/sshd_config
