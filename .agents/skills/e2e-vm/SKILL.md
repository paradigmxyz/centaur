---
name: e2e-vm
description: "Run the e2e/ suite in a disposable local LXD VM that mirrors the CI runner (native k3s), so a developer host's own k3s or dev stack is never touched. Use for creating, provisioning, starting, syncing code into, testing in, or stopping that VM."
---

# E2E VM

`e2e/stack.sh` deploys into the host's k3s. CI installs k3s on a throwaway
runner; on a development host that already runs k3s (or must not), use an LXD
VM instead. It has its own kernel, so k3s, Docker, and iptables inside it stay
isolated from the host.

## Ground rules

- Never run `e2e/stack.sh` against the host's own k3s unless the user asks.
- Run every `lxc` command with `sudo` unless the user is in the `lxd` group in
  the current shell.
- Keep the VM's k3s version and flags identical to the `Install k3s` step of
  the `e2e` job in `.github/workflows/publish-images.yml`.
- Stop the VM when done; delete it only when asked.

## Start an existing VM

```bash
sudo lxc list centaur-e2e
sudo lxc start centaur-e2e
```

k3s starts with the VM. Then sync the code and run the suite (below).

## Create and provision (first time)

One-time host setup. `lxd init --minimal` creates the `lxdbr0` bridge and a
`dir` storage pool:

```bash
sudo snap install lxd   # if `snap list lxd` shows nothing
sudo lxd init --minimal
```

If Docker runs on the host, its `FORWARD DROP` policy blocks the VM's internet
access. Allow the bridge (not persistent across reboots):

```bash
sudo iptables -I DOCKER-USER -i lxdbr0 -j ACCEPT
sudo iptables -I DOCKER-USER -o lxdbr0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
```

Create the VM at the CI runner's size:

```bash
sudo lxc launch ubuntu:24.04 centaur-e2e --vm \
  -c limits.cpu=8 -c limits.memory=32GiB -d root,size=200GiB
sudo lxc exec centaur-e2e -- cloud-init status --wait
```

Provision it like the CI runner. Copy `INSTALL_K3S_VERSION` and
`INSTALL_K3S_EXEC` from the workflow's `Install k3s` step:

```bash
sudo lxc exec centaur-e2e -- bash -c '
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq docker.io docker-buildx git jq openssl curl unzip
usermod -aG docker ubuntu
curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION=<version> \
  INSTALL_K3S_EXEC="<flags>" sh -
curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
curl -fsSL https://just.systems/install.sh | bash -s -- --to /usr/local/bin
su - ubuntu -c "curl -fsSL https://bun.sh/install | bash"
ln -sf /home/ubuntu/.bun/bin/bun /usr/local/bin/bun
'
```

## Sync the working tree

Copy tracked and untracked, non-ignored files, so uncommitted changes are
included:

```bash
git ls-files -co --exclude-standard -z | tar --null -T - -czf /tmp/centaur-src.tgz
sudo lxc file push /tmp/centaur-src.tgz centaur-e2e/home/ubuntu/centaur-src.tgz
sudo lxc exec centaur-e2e -- bash -c '
rm -rf /home/ubuntu/centaur && mkdir /home/ubuntu/centaur &&
tar xzf /home/ubuntu/centaur-src.tgz -C /home/ubuntu/centaur &&
chown -R ubuntu:ubuntu /home/ubuntu/centaur'
```

## Run the suite

Run as the `ubuntu` user. Use published images when the change does not touch
an image (fast); `main-sha-<sha7>` and `sha-<sha7>` tags exist for published
commits:

```bash
vm() { sudo lxc exec centaur-e2e --user 1000 --group 1000 \
  --env HOME=/home/ubuntu --cwd /home/ubuntu/centaur -- bash -c "$1"; }
vm 'CENTAUR_E2E_IMAGE_TAG=sha-<sha7> e2e/stack.sh up'
vm 'CENTAUR_E2E_CONCURRENCY=8 e2e/stack.sh test'
```

Without `CENTAUR_E2E_IMAGE_TAG`, `up` builds every image inside the VM and
imports it into k3s. The first build has a cold cache and takes a long time.
Run it under `sg docker -c '...'` if the docker group is not yet active.

Run `vm 'e2e/stack.sh test e2e/tests/<file>.test.ts'` for one file,
`vm 'e2e/stack.sh logs'` for diagnostics, and `vm 'e2e/stack.sh down'` to
remove the stack while keeping the VM. For a long run, start it under `nohup`
writing to a log file in the VM and poll the log, so an interrupted host command
does not kill the run.

## Inspect

```bash
sudo lxc exec centaur-e2e -- k3s kubectl -n centaur-e2e get pods
sudo lxc exec centaur-e2e -- k3s kubectl -n centaur-e2e logs deploy/centaur-centaur-slackbotv2
```

For durable state, query Postgres with `psql` inside
`sts/centaur-centaur-postgres`. Use the `POSTGRES_PASSWORD` key of the
`centaur-infra-env` Secret and do not print it.

## Stop or remove

```bash
sudo lxc stop centaur-e2e                 # keeps the disk and image cache
sudo lxc delete --force centaur-e2e       # only when asked
```
