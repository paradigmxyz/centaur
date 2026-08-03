#!/usr/bin/env bash
# Bootstrap a minimal Ubuntu LTS host for a Centaur k3s deployment.
set -euo pipefail

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

if ! command -v apt-get >/dev/null; then
  echo "This bootstrap script supports Ubuntu LTS hosts with apt-get." >&2
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive
${SUDO} apt-get update
${SUDO} apt-get install -y ca-certificates curl docker.io jq just

${SUDO} systemctl enable --now docker
if ! groups "${USER}" | grep -qw docker; then
  ${SUDO} usermod -aG docker "${USER}"
  echo "Added ${USER} to the docker group; sign out and back in before using Docker without sudo."
fi

if ! command -v kubectl >/dev/null; then
  kubectl_version="$(curl -fsSL https://dl.k8s.io/release/stable.txt)"
  curl -fsSLo /tmp/kubectl "https://dl.k8s.io/release/${kubectl_version}/bin/linux/amd64/kubectl"
  curl -fsSLo /tmp/kubectl.sha256 "https://dl.k8s.io/release/${kubectl_version}/bin/linux/amd64/kubectl.sha256"
  echo "$(cat /tmp/kubectl.sha256)  /tmp/kubectl" | sha256sum --check
  ${SUDO} install -m 0755 /tmp/kubectl /usr/local/bin/kubectl
  rm -f /tmp/kubectl /tmp/kubectl.sha256
fi

if ! command -v helm >/dev/null; then
  curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
fi

if ! command -v k3s >/dev/null; then
  curl -fsSL https://get.k3s.io | ${SUDO} sh -
fi

${SUDO} install -d -m 0700 "${HOME}/.kube"
${SUDO} cp /etc/rancher/k3s/k3s.yaml "${HOME}/.kube/config"
${SUDO} chown "$(id -u):$(id -g)" "${HOME}/.kube/config"
chmod 0600 "${HOME}/.kube/config"

cat <<'EOF'

Bootstrap complete.

Next steps:
1. Follow spaces/docs/phase0/droplet-migration.md from the repository checkout.
2. Clone the approved revision and configure a secret source using placeholders;
   do not put credentials in this script or shell history.
3. Configure public HTTPS before registering OAuth applications.
4. Set deployment-specific values such as <CENTAUR_NAMESPACE>,
   <CENTAUR_RELEASE>, and <CENTAUR_CONSOLE_PUBLIC_URL>.
EOF
