# Centaur 与阿里云 ACK 兼容性核验

调研时间：2026-08-11

## 结论

**可以部署，但不是不改配置就能直接用于生产。** 推荐底座是：

> ACK 托管集群 Pro 版 + 普通 ECS/containerd 节点 + Terway 共享 ENI 并开启 NetworkPolicy + 云盘 RWO + NAS RWX + ACR + ALB HTTPS。

不建议第一阶段选择 ACK Serverless、Auto Mode 智能托管节点、ACK 安全沙箱 runV，或直接照 Centaur 文档填写 `runtimeClassName: gvisor`。先在普通 containerd 节点跑通并做网络策略验证，再评估更强运行时隔离。

## 兼容性矩阵

| 项目 | 结论 | 核验依据与动作 |
| --- | --- | --- |
| 集群类型 | **匹配，推荐 ACK 托管 Pro** | Centaur 会安装控制器、CRD、ClusterRole，并动态创建 Pod/PVC/Service/NetworkPolicy；普通 ACK 托管集群完整支持这些原生资源。ACK 官方已停止新建专有集群，生产场景推荐托管 Pro。[ACK 集群创建说明](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/create-an-ack-managed-cluster-2) |
| Kubernetes 版本 | **没有代码声明的硬性下限，需做预发验证** | Centaur Chart 没有 `kubeVersion`，资源 API 使用 `apps/v1`、`networking.k8s.io/v1`、`apiextensions.k8s.io/v1`。截至调研日 ACK 在维护版本包括 1.35、1.36；建议先选 **1.35**，并对 `agent-sandbox-controller:v0.4.6` 做完整冒烟测试。[ACK 版本支持表](https://help.aliyun.com/zh/ack/product-overview/release-notes-for-kubernetes-versions)；[Chart.yaml](../contrib/chart/Chart.yaml) |
| CRD / 集群权限 | **匹配，但安装者必须有集群级权限** | 子 Chart 自动安装 `sandboxes.agents.x-k8s.io` 等 CRD，并创建 ClusterRole/ClusterRoleBinding；namespace-only 权限不够。ACK 对集群资源操作同时使用 RAM 与 Kubernetes RBAC，安装账号应具备相应 RAM 权限及集群管理员 RBAC。[ACK RBAC 授权](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/grant-rbac-permissions-to-ram-users-or-ram-roles)；[controller RBAC](../contrib/chart/charts/agent-sandbox/templates/rbac.generated.yaml)；[CRD](../contrib/chart/charts/agent-sandbox/crds/agents.x-k8s.io_sandboxes.yaml) |
| CNI / NetworkPolicy | **有条件匹配，只选 Terway 共享 ENI** | Centaur 默认全局 deny，并依赖大量标准 NetworkPolicy；ACK 仅在 Terway 共享 ENI 节点支持该能力，Flannel和 Terway 独占 ENI 不适合。[ACK NetworkPolicy 限制](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/use-network-policies)；[Centaur default-deny](../contrib/chart/templates/networkpolicy.yaml) |
| PostgreSQL 存储 | **匹配，使用云盘 RWO** | Chart 默认申请 20 GiB、`ReadWriteOnce` PVC。ACK 云盘正是 RWO，建议显式设置多可用区友好的 ESSD StorageClass；ACK 不默认提供默认 StorageClass，留空可能使 PVC Pending。[ACK 存储基础](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/storage-basics)；[PostgreSQL PVC](../contrib/chart/templates/workloads.yaml) |
| Repo Cache | **匹配，生产建议 NAS RWX** | Centaur 默认用节点 `hostPath` + DaemonSet，适合固定节点或试用；多节点生产建议切换为 PVC，Chart 明确要求跨节点时使用 RWX。ACK NAS 支持多节点共享读写。[ACK NAS](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/nas-volume-overview-1)；[repo cache 配置](../contrib/chart/values.yaml#L294) |
| 公网 HTTPS | **匹配，使用 ALB Ingress** | Centaur 生成标准 Ingress，可设置 class、annotations 与 TLS；ACK ALB Ingress 支持公网入口及 HTTPS 证书。需先安装/配置 ALB Ingress Controller、AlbConfig 和自有域名。[ACK ALB Ingress](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/create-and-use-alb-ingress-to-expose-services-to-the-public)；[ACK HTTPS 证书](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/use-an-alb-ingress-to-configure-certificates-for-an-https-listener)；[Centaur Ingress](../contrib/chart/templates/ingress.yaml) |
| 镜像 | **匹配，但应复制到 ACR** | Centaur 主分支和发行标签为自身 9 个镜像构建 amd64/arm64 manifest，但 Chart 默认仓库名是本地短名称；ACK 中国区直接拉 GHCR、Docker Hub、`registry.k8s.io` 还可能失败。应将 Centaur、agent-sandbox controller、ParadeDB 及可选 1Password 镜像复制到同地域 ACR，并覆盖全部 repository/tag。[多架构构建](../.github/workflows/publish-images.yml#L41)；[ACK 海外镜像建议](https://help.aliyun.com/zh/ack/product-overview/announcement-on-ack-cluster-unable-to-pull-overseas-source-mirror)；[ACR 对接](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/container-registry) |
| API Server 端口 | **默认值匹配 ACK** | Centaur 默认允许 `6443`；ACK 托管集群 API Server 的私网监听也是 6443。保留 `networkPolicy.apiServerPort: 6443`，只有实际集群端点证实为其他端口时再改。[ACK API Server ACL](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/configure-network-acls-for-the-api-server-of-an-ack-cluster)；[Centaur 配置](../contrib/chart/values.yaml#L694) |
| RuntimeClass / 强隔离 | **接口兼容，默认方案不匹配** | Centaur 接受任意 `runtimeClassName`，文档示例是 `gvisor`；ACK 原生安全沙箱名称是 `runv`，并要求弹性裸金属、Alibaba Cloud Linux 3，且不支持 Terway DataPath V2。首期应留空，使用普通 containerd。需要 runV 时单建节点池并重新验证网络和卷挂载。[ACK runV 要求](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/node-pool-management-in-sandboxed-containers)；[ACK RuntimeClass 示例](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/differences-between-runc-and-runv)；[Centaur runtime 配置](../contrib/chart/values.yaml#L245) |

## 两个生产阻断项

### 1. Terway 的 `ipBlock.except` 必须专项验证

Centaur 给每个 iron-proxy 生成 `0.0.0.0/0`，再用 `except` 排除 RFC1918、链路本地和保留网段，以阻止代理访问集群/内网地址：[iron_proxy.rs](../services/api-rs/crates/centaur-sandbox-agent-k8s/src/iron_proxy.rs#L1635)。

ACK 官方明确提示，Terway DataPath V2 对 NetworkPolicy 的 `except` 关键字支持不佳，不建议使用：[Terway 工作模式](https://help.aliyun.com/zh/ack/ack-managed-and-ack-dedicated/user-guide/work-with-terway)。因此，**不能在未测试时把当前 NetworkPolicy 当成可靠的内网隔离边界**。

上线前至少验证 sandbox/iron-proxy 无法访问：

- ACK Pod、Service、Node 和 VPC CIDR；
- `169.254.169.254` 实例元数据；
- RDS、Redis、内部 HTTP 服务；
- 非授权 namespace 的 Service。

同时把 ACK 实际 Pod/Service/VPC CIDR 加入 `ironProxy.upstreamDenyCidrs`，作为应用层补充。若测试不满足隔离要求，应改用经过验证的 BYOCNI/Cilium，或调整 Centaur 策略实现后再上线。

### 2. ALB 入站需要补充 NetworkPolicy

Centaur 的 Slackbot 入站策略默认只允许来自配置 namespace 的 Pod：[networkpolicy.yaml](../contrib/chart/templates/networkpolicy.yaml#L349)。ALB 的业务流量并不是由 ALB Controller Pod 代理转发，因此仅设置 `ingressControllerNamespaces: [kube-system]` 不足以证明流量可达。

这是根据双方数据路径做出的推断。部署时应为 Slackbot/Console 增加一条独立 NetworkPolicy，显式允许 ALB 后端访问所使用的源网段和端口，并用公网 HTTPS 真实回调做验证；不要直接关闭 Centaur 的全部 NetworkPolicy。

## 推荐 ACK 配置

1. 创建 ACK 托管集群 Pro 版，选择当前维护中的 Kubernetes 1.35、普通 ECS/containerd 节点。
2. 网络选择 Terway 共享 ENI并开启 NetworkPolicy；先不要启用 runV 或填写 `gvisor`。
3. 安装并授权 CSI、ALB Ingress Controller；准备 ALB、域名及证书。
4. PostgreSQL 使用 ESSD 云盘 RWO；repo cache 使用 NAS/CNFS RWX。
5. 将全部镜像同步到同地域 ACR，优先统一使用 amd64 节点，稳定后再评估 Arm 混部。
6. 使用具备集群管理员 RBAC 的发布身份安装 Helm；安装后再收敛日常运维权限。
7. 保留 `networkPolicy.apiServerPort: 6443`，并为 ALB 入站补充独立策略。
8. 完成 CRD、沙箱创建/暂停/恢复、PVC、镜像拉取、Slack 回调、NetworkPolicy 隔离和节点故障迁移的预发测试。

建议的关键 values 方向（镜像和 StorageClass 名称需替换为实际 ACK/ACR 资源）：

```yaml
global:
  imagePullSecrets:
    - name: acr-pull-secret

sandbox:
  runtimeClassName: ""
  nodeSelector:
    kubernetes.io/arch: amd64
  stateVolume:
    enabled: true
    storageClassName: alicloud-disk-topology-alltype

postgres:
  persistence:
    storageClassName: alicloud-disk-topology-alltype

repoCache:
  storage:
    type: persistentVolumeClaim
    persistentVolumeClaim:
      create: true
      accessModes:
        - ReadWriteMany
      storageClassName: <ACK-NAS-RWX-StorageClass>
      size: 20Gi

ingress:
  enabled: true
  className: alb
  host: centaur.example.com
  tls:
    - hosts:
        - centaur.example.com
      secretName: centaur-tls

networkPolicy:
  enabled: true
  ingressControllerNamespaces:
    - kube-system
  apiServerPort: 6443
```

此片段没有包含 ALB 的 AlbConfig/annotation、额外入站 NetworkPolicy、Secret 和完整镜像覆盖，不能直接作为生产 values 使用。
