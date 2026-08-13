# 中国版飞书 Console OAuth 登录契约

核验日期：2026-08-13

## 范围与结论

本文只采用中国版飞书开放平台 `open.feishu.cn` / `accounts.feishu.cn` 官方文档，不使用 Lark 国际站资料。

Console 应实现标准 OAuth 2.0 Authorization Code 流程，并采用当前 **v3 token endpoint**。官方已将 v2 token endpoint 标记为历史版本、不推荐使用。

```text
GET  https://accounts.feishu.cn/open-apis/authen/v1/authorize
POST https://accounts.feishu.cn/oauth/v3/token
GET  https://open.feishu.cn/open-apis/authen/v1/user_info
```

来源：[获取授权码](https://open.feishu.cn/document/authentication-management/access-token/obtain-oauth-code)；[获取 user_access_token（当前 v3）](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/authentication-management/access-token/get-user-access-token-v3)；[获取用户信息](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/authen-v1/user_info/get)。

## 1. Authorization endpoint

```http
GET https://accounts.feishu.cn/open-apis/authen/v1/authorize
```

| 参数 | 要求 |
| --- | --- |
| `client_id` | 必填，飞书应用 App ID。 |
| `response_type` | 必填，固定为 `code`。 |
| `redirect_uri` | 必填，URL 编码；必须预先配置在应用“安全设置”的重定向 URL 列表中。 |
| `scope` | 可选，空格分隔且区分大小写；请求的 scope 必须先在应用后台开通。最多一次请求 200 个。需要 refresh token 时加入 `offline_access`。 |
| `state` | 可选，但 Console 必须生成并校验，用于关联请求并防止 CSRF。 |
| `code_challenge` | 可选；启用 PKCE 时传入。 |
| `code_challenge_method` | 可选，`S256`（官方推荐）或 `plain`（默认）。 |
| `prompt` | 可选；当前文档给出的值为 `consent`，用于显式展示授权页。 |

成功回调为 `redirect_uri?code=...&state=...`；授权码有效期 5 分钟且只能使用一次。用户拒绝时回调 `error=access_denied&state=...`。

来源：[获取授权码：请求参数与响应](https://open.feishu.cn/document/authentication-management/access-token/obtain-oauth-code)。

## 2. Token exchange

当前契约：

```http
POST https://accounts.feishu.cn/oauth/v3/token
Content-Type: application/json; charset=utf-8

{
  "grant_type": "authorization_code",
  "client_id": "<app-id>",
  "client_secret": "<app-secret>",
  "code": "<authorization-code>",
  "redirect_uri": "<same-callback-uri>",
  "code_verifier": "<pkce-verifier-if-used>"
}
```

- `grant_type`、`client_id`、`code` 必填。
- 通过飞书开放平台创建的应用目前均为 Confidential Client，因此 Console 必须传 `client_secret`。官方说明 Public Client 尚不开放注册，仅飞书官方 MCP 应用属于该类型。
- `redirect_uri` 在 token 请求中标为可选，但一旦传入必须与授权阶段一致。Console 应始终传入同一个精确值，减少配置歧义。
- 使用 PKCE 时 `code_verifier` 必填，长度 43–128，字符集为字母、数字及 `-._~`。
- 可选 `scope` 只能缩减已授权权限，不可扩大；不传则包含授权阶段获得的全部权限。

成功响应：

```json
{
  "code": 0,
  "access_token": "...",
  "expires_in": 7200,
  "refresh_token": "...",
  "refresh_token_expires_in": 604800,
  "token_type": "Bearer",
  "scope": "..."
}
```

有效期不是稳定常量，必须按响应字段计算。`refresh_token` 和 `refresh_token_expires_in` 仅在授权了 `offline_access` 时返回；refresh token 只能使用一次。官方提醒 token 通常为 1–2 KB，并建议存储预留 4 KB。

错误响应同时包含 HTTP 状态和 JSON：

```json
{
  "code": 20050,
  "error": "server_error",
  "error_description": "An unexpected server error occurred. Please retry your request."
}
```

常见 HTTP 状态为 `400`、`500` 或 `503`；业务错误码包括无效/过期/已用授权码、无应用使用权限、PKCE 失败、redirect URI 不匹配等。实现不能只检查 HTTP 2xx，也不能只检查 `code`。

来源：[获取 user_access_token（v3）](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/authentication-management/access-token/get-user-access-token-v3)。旧 [v2 文档](https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token) 明确写明已成为历史版本，并指出端点已从 `https://open.feishu.cn/open-apis/authen/v2/oauth/token` 迁移至 v3。

## 3. User info

```http
GET https://open.feishu.cn/open-apis/authen/v1/user_info
Authorization: Bearer <user_access_token>
```

响应外层采用飞书 OpenAPI 形式：

```json
{
  "code": 0,
  "msg": "success",
  "data": {
    "open_id": "ou-...",
    "union_id": "on-...",
    "tenant_key": "...",
    "email": "user@example.com",
    "enterprise_email": "user@corp.example"
  }
}
```

| 字段 | 官方语义与身份用途 |
| --- | --- |
| `open_id` | 用户在当前应用内的唯一标识。可作为 `(app_id, open_id)` 下的稳定外部标识。 |
| `union_id` | 用户对同一 ISV 的唯一标识；同一 ISV 名下应用间保持一致。它不是脱离 ISV 语境的全球 ID。 |
| `tenant_key` | 当前企业标识。Console 的飞书租户身份应直接来自 user-info 返回值，而不是从邮箱域名、回调 host 或配置猜测。 |
| `email` | 用户邮箱；需要 `contact:user.email:readonly`。官方明确说明这是管理员导入的联系方式，未由用户实时验证，不建议直接作为业务系统登录凭证。 |
| `enterprise_email` | 企业邮箱；要求企业在管理后台启用飞书邮箱，并需要 `contact:user.employee:readonly`。并非所有用户或租户都有值。 |

其他返回字段包括姓名、头像、`user_id`、手机号、工号；敏感字段各自受字段权限控制。

来源：[获取用户信息：响应字段和字段权限](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/authen-v1/user_info/get)。

## 4. Console 身份键

飞书契约提供三种不同作用域的标识。结合 Centaur 已批准的跨应用身份设计，Console 使用以下映射：

```text
provider    = "feishu"
tenant_id  = tenant_key
subject_id = canonical_json([tenant_key, union_id])
delivery   = (tenant_key, open_id)
```

`union_id` 只在同一 ISV 范围内跨应用稳定，因此它必须和 `tenant_key` 一起编码，不能单独作为主体。`open_id` 只用于把 Console 用户关联回当前机器人应用的投递身份。不要以 `email` 为主键，也不要通过邮箱域名推断 `tenant_key`。

这是 Centaur 的产品身份决策，而不是飞书规定的唯一建模方式；底层字段作用域来自上面的官方契约。

## 5. Scope 与 PKCE

**已核验：**

- 授权阶段 `scope` 是空格分隔的增量权限列表；请求的权限必须先由应用开通。
- `offline_access` 决定是否返回 refresh token。
- PKCE 受支持，授权端支持 `code_challenge` 和 `S256`/`plain`，token 端支持 `code_verifier`。
- v3 修复了 v2 的 PKCE 兼容性问题：授权阶段未传 `code_challenge`、换 token 时却传 `code_verifier`，v3 会按 PKCE 语义拒绝。
- 对当前 Console 这种服务器端 Confidential Client，官方并未要求必须启用 PKCE，但启用 `S256` 可增加授权码拦截防护；`client_secret` 仍然必须服务端保存和发送。

来源：[获取授权码](https://open.feishu.cn/document/authentication-management/access-token/obtain-oauth-code)；[v3 token](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/authentication-management/access-token/get-user-access-token-v3)；[v2 迁移说明](https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token)。

## 6. 已核验与不确定项

### 已核验

- 当前 token endpoint 是 `https://accounts.feishu.cn/oauth/v3/token`，不是 v2 endpoint。
- authorization endpoint、完整参数、PKCE、拒绝授权回调和 5 分钟单次授权码契约。
- user-info endpoint 及 `open_id`、`union_id`、`tenant_key`、`email`、`enterprise_email` 字段。
- 企业租户身份可由 `tenant_key` 获得。
- 普通邮箱和企业邮箱均有权限/配置条件；普通邮箱不可视为经过实时验证的认证凭据。
- token endpoint 使用 OAuth 风格 `error`/`error_description` 加飞书数值 `code`；user-info 使用 `code`/`msg`/`data`。

### 仍不应假设

- **不能保证拿到邮箱。** `email` 需要用户邮箱字段权限且可能为空；`enterprise_email` 还要求租户启用飞书邮箱服务。
- **不能把邮箱当作可信身份断言。** 官方明确警告其未实时验证；Console 应以飞书 ID 组合做身份键，邮箱只用于展示或经过本系统额外验证后的账号关联。
- **不能假设 `union_id` 跨所有开发者/企业全球稳定。** 官方只保证同一 ISV 名下应用一致。
- **不能假设 token 有效期固定为示例中的 7200/604800 秒。** 必须按响应值。
- **不能使用旧 v2 教程中的 endpoint 作为新实现契约。** 当前 v3 文档才是实现依据。

## 官方来源

- [获取授权码](https://open.feishu.cn/document/authentication-management/access-token/obtain-oauth-code)
- [获取 user_access_token（v3）](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/authentication-management/access-token/get-user-access-token-v3)
- [v2 token 历史版本与迁移说明](https://open.feishu.cn/document/authentication-management/access-token/get-user-access-token)
- [获取用户信息](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/authen-v1/user_info/get)
- [浏览器网页接入指南](https://open.feishu.cn/document/common-capabilities/sso/web-application-end-user-consent/guide)
