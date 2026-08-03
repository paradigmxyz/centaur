# OAuth app registration checklist

Register the applications only after the deployment's egress-domain allowlist
is active. Do not register production client secrets until that allowlist is
deployed.

Set `CENTAUR_CONSOLE_PUBLIC_URL` to the public console host name (without a
scheme in the templates below). The Centaur OAuth app configuration uses a
unique slug:

- HubSpot: `hubspot`
- Microsoft Entra ID: `microsoft`

For either provider, register these URLs:

```text
Redirect URL: https://<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/callback
Start URL:    https://<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/start
```

The start URL is a user-facing consent link. The redirect URL is configured at
the provider and must use the same public host, path, and slug as the Centaur
OAuth app.

## HubSpot

1. In the HubSpot developer portal, create an app for the intended environment.
2. Add the redirect URL with the `hubspot` slug.
3. Configure the app for OAuth authorization-code flow and request only the
   read paths required for Sales Scout:
   - `crm.objects.contacts.read`
   - `crm.objects.companies.read`
   - `crm.objects.deals.read`
   - `oauth`
4. Record the client ID in the table below.
5. After the egress allowlist is deployed, create the matching Centaur OAuth
   app and store the client secret through the configured secret-management
   flow. Never add it to repository files, shell history, or this document.
6. Complete consent through the start URL and confirm the resulting credential
   is scoped to the intended principal.

## Microsoft Entra ID

1. In Microsoft Entra admin center, create an App registration for the intended
   environment and choose the supported account audience for the deployment.
2. Add a **Web** redirect URI using the `microsoft` slug.
3. Add delegated Microsoft Graph permissions for Sales Scout read paths:
   - `User.Read`
   - `Mail.Read`
   - `Calendars.Read`
   - `offline_access`
4. Grant tenant admin consent only when the deployment policy requires it.
5. Record the Application (client) ID and tenant choice below.
6. After the egress allowlist is deployed, create the matching Centaur OAuth
   app and store the client secret through the configured secret-management
   flow. Never place the secret in this repository.
7. Complete consent through the start URL and confirm the resulting credential
   is scoped to the intended principal.

## Application record (client IDs only)

| Provider | Environment | OAuth slug | Client ID | Tenant / account audience | Registered by | Date |
|---|---|---|---|---|---|---|
| HubSpot | `<environment>` | `hubspot` | `<client-id>` | `<account>` | `<owner>` | `<yyyy-mm-dd>` |
| Microsoft Entra ID | `<environment>` | `microsoft` | `<client-id>` | `<tenant-or-audience>` | `<owner>` | `<yyyy-mm-dd>` |

Do not record client secrets in this table.
