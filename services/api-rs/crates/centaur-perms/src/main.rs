//! `centaur-perms` — manage iron-control permissions for Centaur: which chat
//! principals (Slack, Discord, Teams) and roles hold which tool roles and secrets.
//!
//! Commands are resource-first: `centaur-perms <noun> <verb>`, where the noun is
//! `principals`, `roles`, or `secrets`. The CLI reuses `centaur-iron-control`'s
//! canonical mappings (`derive_principal`, `RoleSpec::tool`) so the principal
//! and role `foreign_id`s it writes match exactly what api-rs registers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use centaur_iron_control::{
    BrokerCredentialInput, Grant, GrantSecret, Grantee, IdentityInput, IronControlClient,
    IronControlError, Role, RoleSpec, SECRET_TYPES, grant_inputs_to_role, managed_labels,
};
use centaur_iron_proxy::SourcePolicy;
use clap::{Args, Parser, Subcommand, ValueEnum};
use eyre::{Result, bail};

mod principal;
mod tools;
mod translate;

use tools::ParsedSecret;

#[cfg(test)]
mod tests;

#[derive(Parser, Debug)]
#[command(
    name = "centaur-perms",
    about = "Manage iron-control permissions: grant principals and roles access to tools and secrets"
)]
struct Cli {
    /// iron-control admin API base URL.
    #[arg(long, env = "IRON_CONTROL_URL")]
    iron_control_url: String,

    /// iron-control admin API key (`iak_…`).
    #[arg(long, env = "IRON_CONTROL_API_KEY")]
    iron_control_api_key: String,

    /// iron-control namespace.
    #[arg(long, env = "IRON_CONTROL_NAMESPACE", default_value = "default")]
    namespace: String,

    /// Tool directory to search for `--tool` names. Repeatable; later
    /// directories shadow earlier ones (overlay order). The colon-separated
    /// `TOOL_DIRS` env var is appended after any `--tools-dir` values.
    #[arg(long = "tools-dir", value_name = "DIR")]
    tools_dirs: Vec<PathBuf>,

    /// How a tool secret's `secret_ref` is resolved into an iron-control source.
    #[arg(long, value_enum, default_value_t = SourcePolicyArg::Env)]
    source_policy: SourcePolicyArg,

    /// 1Password vault (required for `--source-policy onepassword*`).
    #[arg(long)]
    op_vault: Option<String>,

    /// 1Password item TTL.
    #[arg(long, default_value = "10m")]
    op_ttl: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SourcePolicyArg {
    Env,
    Onepassword,
    OnepasswordConnect,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Inspect principals and manage what they can access.
    #[command(subcommand)]
    Principals(PrincipalsCmd),
    /// Inspect roles and manage the secrets attached to them.
    #[command(subcommand)]
    Roles(RolesCmd),
    /// Inspect the secrets registered in iron-control.
    #[command(subcommand)]
    Secrets(SecretsCmd),
    /// Manage iron-control broker credentials (managed OAuth refresh tokens
    /// delivered inline to proxies via a `token_broker` source).
    #[command(subcommand)]
    Broker(BrokerCmd),
}

#[derive(Subcommand, Debug)]
enum PrincipalsCmd {
    /// List principals registered in iron-control.
    List(FilterArgs),
    /// Show the roles, grants, and effective secrets a principal resolves to.
    Show(PrincipalSelector),
    /// Grant a principal access to tools, roles, and/or secrets.
    Grant(PrincipalGrantArgs),
    /// Revoke a principal's access to tools, roles, secrets, and/or grants.
    Revoke(PrincipalGrantArgs),
}

#[derive(Subcommand, Debug)]
enum RolesCmd {
    /// List roles registered in iron-control.
    List(FilterArgs),
    /// Show the secrets granted to a role.
    Show(RoleSelector),
    /// Grant secrets to a role, by OID or sourced from a tool's config.
    Grant(RoleGrantArgs),
    /// Revoke one or more secrets from a role.
    Revoke(RoleSecretArgs),
}

#[derive(Subcommand, Debug)]
enum SecretsCmd {
    /// List secrets of every type registered in iron-control.
    List(FilterArgs),
    /// Show one secret's full configuration. Credential values are never
    /// returned by iron-control — only the source each resolves from.
    Show(SecretSelector),
}

#[derive(Args, Debug)]
struct SecretSelector {
    /// Secret OID (`ssr_`/`ots_`/`gas_`/`gid_`/`pgs_`/`hms_`/`aas_`) or
    /// `foreign_id`. A `foreign_id` is resolved by trying each secret type in turn.
    secret: String,
}

#[derive(Subcommand, Debug)]
enum BrokerCmd {
    /// Create or update a broker credential. iron-control owns the OAuth refresh
    /// loop and delivers the current access token inline to proxies. Values are
    /// passed literally; re-supplying `--refresh-token` re-bootstraps the
    /// credential.
    Create(Box<BrokerCreateArgs>),
    /// List broker credentials registered in iron-control.
    List(FilterArgs),
    /// Show one broker credential's full record (status/expiry/etc.). Secret
    /// material is never returned by iron-control.
    Show(BrokerSelector),
    /// Delete a broker credential.
    Delete(BrokerSelector),
}

#[derive(Args, Debug)]
struct BrokerSelector {
    /// Broker credential OID (`bcr_`) or `foreign_id`.
    credential: String,
}

#[derive(Args, Debug)]
struct BrokerCreateArgs {
    /// Stable upsert key for the credential (also what a `token_broker` source
    /// references via `credential_id`), e.g. `openai-codex`.
    #[arg(long)]
    foreign_id: String,

    /// OAuth token endpoint iron-control POSTs the refresh against.
    #[arg(long)]
    token_endpoint: String,

    /// OAuth client id (literal, not secret; echoed back by iron-control).
    #[arg(long)]
    client_id: String,

    /// OAuth client secret (literal; write-only, encrypted at rest). Omit for
    /// public clients.
    #[arg(long)]
    client_secret: Option<String>,

    /// Seed refresh token (literal; write-only). Supplying it (re)bootstraps the
    /// credential and triggers an immediate refresh.
    #[arg(long)]
    refresh_token: Option<String>,

    /// OAuth scope to request. Repeatable.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,

    /// Human-readable name.
    #[arg(long)]
    name: Option<String>,

    /// Human-readable description.
    #[arg(long)]
    description: Option<String>,

    /// Extra header sent on the refresh POST (write-only). `--token-endpoint-header key=value`. Repeatable.
    #[arg(long = "token-endpoint-header", value_name = "KEY=VALUE")]
    token_endpoint_headers: Vec<String>,

    /// Refresh this many seconds before expiry (iron-control default 300).
    #[arg(long)]
    early_refresh_slack_seconds: Option<u64>,

    /// Refresh once this fraction of lifetime remains, in [0,1) (default 0.2).
    #[arg(long)]
    early_refresh_fraction: Option<f64>,

    /// Refresh at least this often (default 86400).
    #[arg(long)]
    max_refresh_interval_seconds: Option<u64>,

    /// Per-attempt token endpoint timeout (default 30).
    #[arg(long)]
    refresh_timeout_seconds: Option<u64>,
}

#[derive(Args, Debug)]
struct FilterArgs {
    /// Only resources carrying this label. Repeatable: `--label key=value`.
    #[arg(long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,

    /// Case-insensitive substring to match against `foreign_id` or name.
    #[arg(long)]
    filter: Option<String>,

    /// Only Centaur-managed resources (label `managed-by=centaur`).
    #[arg(long)]
    managed: bool,
}

#[derive(Args, Debug)]
struct PrincipalSelector {
    /// Slack/Teams/Discord thread key (derived), a principal `foreign_id`
    /// (e.g. `slack-channel-t1-c9`), or an OID (`prn_...`).
    principal: String,

    /// Acting Slack user id, used only to key a DM principal from a thread key.
    #[arg(long)]
    slack_user: Option<String>,
}

#[derive(Args, Debug)]
struct PrincipalGrantArgs {
    /// Slack/Teams/Discord thread key (derived) or raw principal `foreign_id`.
    principal: String,

    /// Acting Slack user id, used only to key a DM principal from a thread key.
    #[arg(long)]
    slack_user: Option<String>,

    /// Tool name — registers its `tool-{slug}` role + secrets, then (un)assigns
    /// it. Repeatable.
    #[arg(long = "tool", value_name = "NAME")]
    tools: Vec<String>,

    /// Existing role `foreign_id` (e.g. `infra`, `tool-github`) to (un)assign.
    /// Repeatable.
    #[arg(long = "role", value_name = "FOREIGN_ID")]
    roles: Vec<String>,

    /// Secret OID (`ssr_`/`ots_`/`gas_`/`gid_`/`pgs_`/`hms_`/`aas_`) to
    /// grant/revoke directly. Repeatable.
    #[arg(long = "secret", value_name = "OID")]
    secrets: Vec<String>,

    /// Grant OID (`grant_…`) to revoke directly. `revoke` only. Repeatable.
    #[arg(long = "grant-id", value_name = "OID")]
    grant_ids: Vec<String>,
}

#[derive(Args, Debug)]
struct RoleSelector {
    /// Role `foreign_id` (e.g. `infra`, `tools`, `tool-github`) or OID.
    role: String,
}

#[derive(Args, Debug)]
struct RoleSecretArgs {
    /// Role `foreign_id` (e.g. `infra`, `tools`, `tool-github`) or OID.
    role: String,

    /// Secret OID (`ssr_`/`ots_`/`gas_`/`gid_`/`pgs_`/`hms_`/`aas_`) to
    /// grant/revoke. Repeatable.
    #[arg(long = "secret", value_name = "OID", required = true)]
    secrets: Vec<String>,
}

#[derive(Args, Debug)]
struct RoleGrantArgs {
    /// Role `foreign_id` (e.g. `infra`, `tools`, `tool-github`) or OID.
    role: String,

    /// Existing secret OID (`ssr_`/`ots_`/`gas_`/`gid_`/`pgs_`/`hms_`/`aas_`) to
    /// grant. Repeatable.
    #[arg(long = "secret", value_name = "OID")]
    secrets: Vec<String>,

    /// Tool name whose `pyproject.toml` secrets to register and grant to the
    /// role. The secret resources keep their canonical `tool-<slug>-…` ids.
    #[arg(long = "tool", value_name = "NAME")]
    tool: Option<String>,

    /// When used with `--tool`, only register the named secret(s) (e.g.
    /// `SLACK_BOT_TOKEN`) instead of all the tool declares. Repeatable.
    #[arg(long = "secret-name", value_name = "NAME", requires = "tool")]
    secret_names: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = IronControlClient::new(&cli.iron_control_url, &cli.iron_control_api_key);

    match &cli.command {
        Command::Principals(cmd) => match cmd {
            PrincipalsCmd::List(args) => principals_list(&cli, &client, args).await,
            PrincipalsCmd::Show(args) => principals_show(&cli, &client, args).await,
            PrincipalsCmd::Grant(args) => principals_grant(&cli, &client, args).await,
            PrincipalsCmd::Revoke(args) => principals_revoke(&cli, &client, args).await,
        },
        Command::Roles(cmd) => match cmd {
            RolesCmd::List(args) => roles_list(&cli, &client, args).await,
            RolesCmd::Show(args) => roles_show(&cli, &client, args).await,
            RolesCmd::Grant(args) => roles_grant(&cli, &client, args).await,
            RolesCmd::Revoke(args) => roles_revoke(&cli, &client, args).await,
        },
        Command::Secrets(cmd) => match cmd {
            SecretsCmd::List(args) => secrets_list(&cli, &client, args).await,
            SecretsCmd::Show(args) => secrets_show(&cli, &client, args).await,
        },
        Command::Broker(cmd) => match cmd {
            BrokerCmd::Create(args) => broker_create(&cli, &client, args).await,
            BrokerCmd::List(args) => broker_list(&cli, &client, args).await,
            BrokerCmd::Show(args) => broker_show(&cli, &client, args).await,
            BrokerCmd::Delete(args) => broker_delete(&cli, &client, args).await,
        },
    }
}

// ---------------------------------------------------------------------------
// principals
// ---------------------------------------------------------------------------

async fn principals_list(cli: &Cli, client: &IronControlClient, args: &FilterArgs) -> Result<()> {
    let labels = filter_labels(args)?;
    let mut found = client.list_principals(&cli.namespace, &labels).await?;
    apply_filter(&mut found, args.filter.as_deref(), |p| {
        (p.foreign_id.clone().unwrap_or_default(), p.name.clone())
    });
    found.sort_by(|a, b| a.foreign_id.cmp(&b.foreign_id));
    print_identities(
        found
            .iter()
            .map(|p| (p.foreign_id.as_deref(), p.id.as_str(), p.name.as_str())),
        &cli.namespace,
        "principal",
    );
    Ok(())
}

/// Describe a grant's secret as ``<type> <name> (<oid>)`` for the show views,
/// resolving the secret's name/foreign_id by OID. Returns `None` for a grant
/// with no secret target. The name lookup is best-effort: if the secret can't
/// be fetched (e.g. it was deleted out from under the grant), the OID is shown
/// alone so a dangling grant still surfaces rather than failing the command.
async fn describe_grant(client: &IronControlClient, grant: &Grant) -> Option<String> {
    let (kind, collection, oid) = grant.secret_target()?;
    let label = match client.get_secret(collection, oid).await {
        Ok(record) => record
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(record.foreign_id.as_deref())
            .unwrap_or(oid)
            .to_owned(),
        Err(_) => oid.to_owned(),
    };
    Some(format!("{kind} {label} ({oid})"))
}

async fn principals_show(
    cli: &Cli,
    client: &IronControlClient,
    args: &PrincipalSelector,
) -> Result<()> {
    let identity =
        principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &cli.namespace, &identity.foreign_id).await?;
    println!(
        "principal: {} ({}) — {}",
        principal.foreign_id.as_deref().unwrap_or("-"),
        principal.id,
        principal.name
    );

    let roles = client.list_principal_roles(&principal.id).await?;
    if roles.is_empty() {
        println!("roles: (none)");
    } else {
        println!("roles:");
        for role in &roles {
            println!(
                "  {} ({})",
                role.foreign_id.as_deref().unwrap_or("-"),
                role.id
            );
            for grant in client.list_role_grants(&role.id).await? {
                if let Some(desc) = describe_grant(client, &grant).await {
                    println!("    grants {desc}");
                }
            }
        }
    }

    let direct = client.list_principal_grants(&principal.id).await?;
    if direct.is_empty() {
        println!("direct grants: (none)");
    } else {
        println!("direct grants:");
        for grant in &direct {
            if let Some(desc) = describe_grant(client, grant).await {
                println!("  {desc} (grant {})", grant.id);
            }
        }
    }

    let effective = client
        .effective_config(&cli.namespace, &principal.id)
        .await?;
    let placeholders: Vec<&str> = effective
        .secrets
        .iter()
        .filter_map(|s| s.replace.as_ref().map(|r| r.proxy_value.as_str()))
        .collect();
    if placeholders.is_empty() {
        println!("effective replace-secrets: (none surfaced)");
    } else {
        println!("effective replace-secrets:");
        for p in placeholders {
            println!("  {p}");
        }
    }
    Ok(())
}

async fn principals_grant(
    cli: &Cli,
    client: &IronControlClient,
    args: &PrincipalGrantArgs,
) -> Result<()> {
    if args.tools.is_empty() && args.roles.is_empty() && args.secrets.is_empty() {
        bail!("nothing to grant: pass at least one --tool, --role, or --secret");
    }
    if !args.grant_ids.is_empty() {
        bail!("--grant-id is only valid for `principals revoke`");
    }
    let policy = build_source_policy(cli)?;
    let identity =
        principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal_id = ensure_principal(client, &identity).await?;
    println!("principal: {} ({principal_id})", identity.foreign_id);

    let dirs =
        tools::resolve_tool_dirs(&cli.tools_dirs, std::env::var("TOOL_DIRS").ok().as_deref());
    for tool in &args.tools {
        let manifest = tools::find_tool(&dirs, tool)?;
        let role = RoleSpec::tool(&manifest.name);
        let tool_labels = translate::ToolLabels {
            tool: manifest.name.clone(),
            overlay: tools::overlay_name_for_tool_dir(&manifest.dir, &dirs),
        };
        let role_id = client
            .upsert_role(&role_identity(&role, &cli.namespace))
            .await?
            .id;
        let secrets: Vec<_> = manifest.all_secrets().cloned().collect();
        let translation = translate::translate_for_tool(
            &cli.namespace,
            &role.foreign_id,
            &tool_labels,
            &secrets,
            &policy,
        );
        let granted = grant_inputs_to_role(client, &role_id, translation.inputs).await?;
        assign_role_idempotent(client, &principal_id, &role_id).await?;
        println!(
            "  tool {} (from {}): role {} ({role_id}) — {} secret(s) registered, role assigned",
            manifest.name,
            manifest.dir.display(),
            role.foreign_id,
            granted.len()
        );
    }

    for role_fid in &args.roles {
        let role = get_role_or_fail(client, &cli.namespace, role_fid).await?;
        assign_role_idempotent(client, &principal_id, &role.id).await?;
        println!("  role {role_fid} ({}): assigned", role.id);
    }

    grant_secrets(
        client,
        &Grantee::Principal(principal_id.clone()),
        &args.secrets,
    )
    .await?;
    Ok(())
}

async fn principals_revoke(
    cli: &Cli,
    client: &IronControlClient,
    args: &PrincipalGrantArgs,
) -> Result<()> {
    if args.tools.is_empty()
        && args.roles.is_empty()
        && args.secrets.is_empty()
        && args.grant_ids.is_empty()
    {
        bail!("nothing to revoke: pass at least one --tool, --role, --secret, or --grant-id");
    }
    let identity =
        principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &cli.namespace, &identity.foreign_id).await?;
    println!("principal: {} ({})", identity.foreign_id, principal.id);

    let assigned = client.list_principal_roles(&principal.id).await?;
    let role_targets = args
        .tools
        .iter()
        .map(|t| (t.as_str(), RoleSpec::tool(t).foreign_id))
        .chain(args.roles.iter().map(|r| (r.as_str(), r.clone())));
    for (label, role_fid) in role_targets {
        match assigned
            .iter()
            .find(|r| r.foreign_id.as_deref() == Some(role_fid.as_str()))
        {
            Some(role) => {
                client.unassign_role(&principal.id, &role.id).await?;
                println!("  {label}: role {role_fid} unassigned");
            }
            None => println!("  {label}: role {role_fid} was not assigned — nothing to do"),
        }
    }

    if !args.secrets.is_empty() {
        let grants = client.list_principal_grants(&principal.id).await?;
        revoke_secrets(
            client,
            &grants,
            &args.secrets,
            "no direct grant on this principal — nothing to do",
        )
        .await?;
    }

    for grant_id in &args.grant_ids {
        client.delete_grant(grant_id).await?;
        println!("  grant {grant_id}: revoked");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// roles
// ---------------------------------------------------------------------------

async fn roles_list(cli: &Cli, client: &IronControlClient, args: &FilterArgs) -> Result<()> {
    let labels = filter_labels(args)?;
    let mut found = client.list_roles(&cli.namespace, &labels).await?;
    apply_filter(&mut found, args.filter.as_deref(), |r| {
        (r.foreign_id.clone().unwrap_or_default(), r.name.clone())
    });
    found.sort_by(|a, b| a.foreign_id.cmp(&b.foreign_id));
    print_identities(
        found
            .iter()
            .map(|r| (r.foreign_id.as_deref(), r.id.as_str(), r.name.as_str())),
        &cli.namespace,
        "role",
    );
    Ok(())
}

async fn roles_show(cli: &Cli, client: &IronControlClient, args: &RoleSelector) -> Result<()> {
    let role = get_role_or_fail(client, &cli.namespace, &args.role).await?;
    println!(
        "role: {} ({}) — {}",
        role.foreign_id.as_deref().unwrap_or("-"),
        role.id,
        role.name
    );
    let grants = client.list_role_grants(&role.id).await?;
    if grants.is_empty() {
        println!("secrets: (none)");
    } else {
        println!("secrets:");
        for grant in &grants {
            if let Some(desc) = describe_grant(client, grant).await {
                println!("  {desc} (grant {})", grant.id);
            }
        }
    }
    Ok(())
}

async fn roles_grant(cli: &Cli, client: &IronControlClient, args: &RoleGrantArgs) -> Result<()> {
    if args.secrets.is_empty() && args.tool.is_none() {
        bail!("nothing to grant: pass at least one --secret <OID> or --tool <NAME>");
    }
    let role = get_role_or_fail(client, &cli.namespace, &args.role).await?;
    println!(
        "role: {} ({})",
        role.foreign_id.as_deref().unwrap_or("-"),
        role.id
    );

    grant_secrets(client, &Grantee::Role(role.id.clone()), &args.secrets).await?;

    if let Some(tool) = &args.tool {
        let policy = build_source_policy(cli)?;
        let dirs =
            tools::resolve_tool_dirs(&cli.tools_dirs, std::env::var("TOOL_DIRS").ok().as_deref());
        let manifest = tools::find_tool(&dirs, tool)?;
        let selected = select_secrets(
            manifest.all_secrets().cloned().collect(),
            &args.secret_names,
        )?;
        // Key the secret resources on the tool's canonical role so the same
        // secret object is shared no matter which role it's granted to.
        let tool_role = RoleSpec::tool(&manifest.name).foreign_id;
        let tool_labels = translate::ToolLabels {
            tool: manifest.name.clone(),
            overlay: tools::overlay_name_for_tool_dir(&manifest.dir, &dirs),
        };
        let translation = translate::translate_for_tool(
            &cli.namespace,
            &tool_role,
            &tool_labels,
            &selected,
            &policy,
        );
        let granted = grant_inputs_to_role(client, &role.id, translation.inputs).await?;
        println!(
            "  tool {} (from {}): {} secret(s) registered and granted to {}",
            manifest.name,
            manifest.dir.display(),
            granted.len(),
            role.foreign_id.as_deref().unwrap_or(&role.id)
        );
    }
    Ok(())
}

/// Pick the named secrets out of a tool's declared set, preserving the order
/// requested. An empty `names` selects them all. Errors if a requested name
/// isn't declared by the tool.
fn select_secrets(all: Vec<ParsedSecret>, names: &[String]) -> Result<Vec<ParsedSecret>> {
    if names.is_empty() {
        return Ok(all);
    }
    let mut selected = Vec::with_capacity(names.len());
    for name in names {
        match all.iter().find(|s| s.name() == name) {
            Some(secret) => selected.push(secret.clone()),
            None => bail!(
                "tool has no secret named {name:?}; declared: {:?}",
                all.iter().map(ParsedSecret::name).collect::<Vec<_>>()
            ),
        }
    }
    Ok(selected)
}

async fn roles_revoke(cli: &Cli, client: &IronControlClient, args: &RoleSecretArgs) -> Result<()> {
    let role = get_role_or_fail(client, &cli.namespace, &args.role).await?;
    println!(
        "role: {} ({})",
        role.foreign_id.as_deref().unwrap_or("-"),
        role.id
    );
    let grants = client.list_role_grants(&role.id).await?;
    revoke_secrets(
        client,
        &grants,
        &args.secrets,
        "not granted to this role — nothing to do",
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// secrets
// ---------------------------------------------------------------------------

async fn secrets_list(cli: &Cli, client: &IronControlClient, args: &FilterArgs) -> Result<()> {
    let labels = filter_labels(args)?;
    // One row per secret across every type: (type, foreign_id, oid, name).
    let mut rows: Vec<(&'static str, Option<String>, String, String)> = Vec::new();
    for (label, collection, _) in SECRET_TYPES {
        match client
            .list_secrets(collection, &cli.namespace, &labels)
            .await
        {
            Ok(found) => rows.extend(
                found
                    .into_iter()
                    .map(|s| (*label, s.foreign_id, s.id, s.name.unwrap_or_default())),
            ),
            // A type that rejects the query (e.g. one that doesn't support a
            // label filter) shouldn't sink the whole cross-type sweep.
            Err(e) => eprintln!("warning: listing {label} secrets failed: {e}"),
        }
    }
    apply_filter(&mut rows, args.filter.as_deref(), |(_, fid, _, name)| {
        (fid.clone().unwrap_or_default(), name.clone())
    });
    rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    print_secrets(&rows, &cli.namespace);
    Ok(())
}

async fn secrets_show(cli: &Cli, client: &IronControlClient, args: &SecretSelector) -> Result<()> {
    let (label, detail) = fetch_secret_detail(client, &cli.namespace, &args.secret).await?;
    println!("secret: {} (type {label})", args.secret);
    println!("{}", serde_json::to_string_pretty(&detail)?);
    Ok(())
}

/// Resolve a secret reference (OID or `foreign_id`) to its `(type label, full
/// resource)`. An OID routes straight to its type by prefix; a `foreign_id` is
/// ambiguous across types, so each type's lookup endpoint is tried in turn
/// until one resolves (404s are skipped).
async fn fetch_secret_detail(
    client: &IronControlClient,
    namespace: &str,
    ident: &str,
) -> Result<(&'static str, serde_json::Value)> {
    if let Some((label, collection, prefix)) = secret_type_for_oid(ident) {
        let detail = client
            .get_secret_detail(collection, prefix, namespace, ident)
            .await?;
        return Ok((label, detail));
    }
    for (label, collection, prefix) in SECRET_TYPES {
        match client
            .get_secret_detail(collection, prefix, namespace, ident)
            .await
        {
            Ok(detail) => return Ok((label, detail)),
            Err(e) if is_status(&e, 404) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!("secret {ident:?} not found in namespace {namespace:?} (tried every secret type)");
}

/// The `(type label, REST collection, OID prefix)` for an OID, matched by
/// prefix. `None` when `ident` is not a recognized secret OID — callers then
/// treat it as a `foreign_id`.
fn secret_type_for_oid(ident: &str) -> Option<(&'static str, &'static str, &'static str)> {
    SECRET_TYPES
        .iter()
        .copied()
        .find(|(_, _, prefix)| ident.starts_with(prefix))
}

fn print_secrets(rows: &[(&str, Option<String>, String, String)], namespace: &str) {
    if rows.is_empty() {
        println!("no secrets found in namespace {namespace:?}");
        return;
    }
    let type_w = rows.iter().map(|(kind, ..)| kind.len()).max().unwrap_or(0);
    let fid_w = rows
        .iter()
        .map(|(_, fid, _, _)| fid.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0);
    for (kind, fid, oid, name) in rows {
        println!(
            "{:<type_w$}  {:<fid_w$}  {}  {}",
            kind,
            fid.as_deref().unwrap_or("-"),
            oid,
            name,
            type_w = type_w,
            fid_w = fid_w,
        );
    }
    println!("({} secret(s))", rows.len());
}

// ---------------------------------------------------------------------------
// broker credentials
// ---------------------------------------------------------------------------

async fn broker_create(
    cli: &Cli,
    client: &IronControlClient,
    args: &BrokerCreateArgs,
) -> Result<()> {
    let token_endpoint_headers = args
        .token_endpoint_headers
        .iter()
        .map(|raw| parse_kv(raw, "--token-endpoint-header"))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let input = BrokerCredentialInput {
        namespace: cli.namespace.clone(),
        foreign_id: args.foreign_id.clone(),
        name: args.name.clone(),
        description: args.description.clone(),
        labels: managed_labels(),
        token_endpoint: args.token_endpoint.clone(),
        scopes: args.scopes.clone(),
        client_id: args.client_id.clone(),
        client_secret: args.client_secret.clone(),
        refresh_token: args.refresh_token.clone(),
        token_endpoint_headers,
        early_refresh_slack_seconds: args.early_refresh_slack_seconds,
        early_refresh_fraction: args.early_refresh_fraction,
        max_refresh_interval_seconds: args.max_refresh_interval_seconds,
        refresh_timeout_seconds: args.refresh_timeout_seconds,
    };
    let record = client.upsert_broker_credential(&input).await?;
    println!(
        "broker credential {} ({}) upserted{}",
        record.foreign_id.as_deref().unwrap_or(&args.foreign_id),
        record.id,
        record
            .status
            .as_deref()
            .map(|s| format!(" — status {s}"))
            .unwrap_or_default(),
    );
    Ok(())
}

async fn broker_list(cli: &Cli, client: &IronControlClient, args: &FilterArgs) -> Result<()> {
    let labels = filter_labels(args)?;
    let mut found = client
        .list_broker_credentials(&cli.namespace, &labels)
        .await?;
    apply_filter(&mut found, args.filter.as_deref(), |c| {
        (
            c.foreign_id.clone().unwrap_or_default(),
            c.name.clone().unwrap_or_default(),
        )
    });
    found.sort_by(|a, b| a.foreign_id.cmp(&b.foreign_id));
    if found.is_empty() {
        println!(
            "no broker credentials found in namespace {:?}",
            cli.namespace
        );
        return Ok(());
    }
    let width = found
        .iter()
        .map(|c| c.foreign_id.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0);
    for c in &found {
        println!(
            "{:<width$}  {}  {}  {}",
            c.foreign_id.as_deref().unwrap_or("-"),
            c.id,
            c.status.as_deref().unwrap_or("-"),
            c.name.as_deref().unwrap_or(""),
            width = width,
        );
    }
    println!("({} broker credential(s))", found.len());
    Ok(())
}

async fn broker_show(cli: &Cli, client: &IronControlClient, args: &BrokerSelector) -> Result<()> {
    let detail = client
        .get_broker_credential_detail(&cli.namespace, &args.credential)
        .await?;
    println!("broker credential: {}", args.credential);
    println!("{}", serde_json::to_string_pretty(&detail)?);
    Ok(())
}

async fn broker_delete(cli: &Cli, client: &IronControlClient, args: &BrokerSelector) -> Result<()> {
    client
        .delete_broker_credential(&cli.namespace, &args.credential)
        .await?;
    println!("broker credential {}: deleted", args.credential);
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn filter_labels(args: &FilterArgs) -> Result<Vec<(String, String)>> {
    let mut labels = args
        .labels
        .iter()
        .map(|l| parse_label(l))
        .collect::<Result<Vec<_>>>()?;
    if args.managed {
        labels.push(("managed-by".to_owned(), "centaur".to_owned()));
    }
    Ok(labels)
}

/// Retain only items whose `foreign_id` or name contains `needle` (case-insensitive).
fn apply_filter<T>(items: &mut Vec<T>, needle: Option<&str>, key: impl Fn(&T) -> (String, String)) {
    if let Some(needle) = needle.map(str::to_lowercase) {
        items.retain(|item| {
            let (fid, name) = key(item);
            fid.to_lowercase().contains(&needle) || name.to_lowercase().contains(&needle)
        });
    }
}

fn print_identities<'a>(
    rows: impl Iterator<Item = (Option<&'a str>, &'a str, &'a str)>,
    namespace: &str,
    noun: &str,
) {
    let rows: Vec<_> = rows.collect();
    if rows.is_empty() {
        println!("no {noun}s found in namespace {namespace:?}");
        return;
    }
    let width = rows
        .iter()
        .map(|(fid, _, _)| fid.unwrap_or("-").len())
        .max()
        .unwrap_or(0);
    for (fid, id, name) in &rows {
        println!(
            "{:<width$}  {}  {}",
            fid.unwrap_or("-"),
            id,
            name,
            width = width
        );
    }
    println!("({} {noun}(s))", rows.len());
}

fn build_source_policy(cli: &Cli) -> Result<SourcePolicy> {
    Ok(match cli.source_policy {
        SourcePolicyArg::Env => SourcePolicy::env(),
        SourcePolicyArg::Onepassword | SourcePolicyArg::OnepasswordConnect => {
            let vault = cli.op_vault.clone().ok_or_else(|| {
                eyre::eyre!("--op-vault is required for --source-policy onepassword*")
            })?;
            if cli.source_policy == SourcePolicyArg::Onepassword {
                SourcePolicy::onepassword(vault, cli.op_ttl.clone())
            } else {
                SourcePolicy::onepassword_connect(vault, cli.op_ttl.clone())
            }
        }
    })
}

fn role_identity(role: &RoleSpec, namespace: &str) -> IdentityInput {
    IdentityInput {
        namespace: namespace.to_owned(),
        foreign_id: role.foreign_id.clone(),
        name: role.name.clone(),
        labels: managed_labels(),
    }
}

/// Grant each secret OID to `grantee`, printing one line per grant.
async fn grant_secrets(
    client: &IronControlClient,
    grantee: &Grantee,
    oids: &[String],
) -> Result<()> {
    for oid in oids {
        let secret = grant_secret_from_oid(oid)?;
        let grant = client.create_grant(grantee, &secret).await?;
        println!("  secret {oid}: granted ({})", grant.id);
    }
    Ok(())
}

/// Revoke each secret OID from `grants` (the grantee's current grants),
/// printing one line per OID. `missing_note` describes the no-op when an OID
/// has no matching grant.
async fn revoke_secrets(
    client: &IronControlClient,
    grants: &[Grant],
    oids: &[String],
    missing_note: &str,
) -> Result<()> {
    for oid in oids {
        match grants.iter().find(|g| g.secret_id() == Some(oid.as_str())) {
            Some(grant) => {
                client.delete_grant(&grant.id).await?;
                println!("  secret {oid}: grant {} revoked", grant.id);
            }
            None => println!("  secret {oid}: {missing_note}"),
        }
    }
    Ok(())
}

fn grant_secret_from_oid(oid: &str) -> Result<GrantSecret> {
    match GrantSecret::from_oid(oid) {
        Some(secret) => Ok(secret),
        None => {
            bail!("--secret expects a secret OID (ssr_/ots_/gas_/gid_/pgs_/hms_/aas_), got {oid:?}")
        }
    }
}

/// Parse a `key=value` label filter.
fn parse_label(raw: &str) -> Result<(String, String)> {
    parse_kv(raw, "--label")
}

/// Parse a `key=value` pair for `flag`, requiring a non-empty key.
fn parse_kv(raw: &str, flag: &str) -> Result<(String, String)> {
    match raw.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_owned(), v.to_owned())),
        _ => bail!("{flag} must be key=value, got {raw:?}"),
    }
}

/// Ensure the principal exists, returning its OID. Looks it up first so an
/// existing principal (e.g. one a session created) is never clobbered; creates
/// it only when absent.
async fn ensure_principal(client: &IronControlClient, identity: &IdentityInput) -> Result<String> {
    match client
        .get_principal(&identity.namespace, &identity.foreign_id)
        .await
    {
        Ok(p) => Ok(p.id),
        Err(e) if is_status(&e, 404) => Ok(client.upsert_principal(identity).await?.id),
        Err(e) => Err(e.into()),
    }
}

async fn get_principal_or_fail(
    client: &IronControlClient,
    namespace: &str,
    ident: &str,
) -> Result<centaur_iron_control::Principal> {
    match client.get_principal(namespace, ident).await {
        Ok(p) => Ok(p),
        Err(e) if is_status(&e, 404) => bail!("principal {ident:?} not found in iron-control"),
        Err(e) => Err(e.into()),
    }
}

async fn get_role_or_fail(client: &IronControlClient, namespace: &str, role: &str) -> Result<Role> {
    match client.get_role(namespace, role).await {
        Ok(r) => Ok(r),
        Err(e) if is_status(&e, 404) => bail!("role {role:?} not found in iron-control"),
        Err(e) => Err(e.into()),
    }
}

/// Assign the role, treating an already-assigned conflict as success.
async fn assign_role_idempotent(
    client: &IronControlClient,
    principal_id: &str,
    role_id: &str,
) -> Result<()> {
    match client.assign_role(principal_id, role_id).await {
        Ok(()) => Ok(()),
        Err(e) if is_status(&e, 409) || is_status(&e, 422) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn is_status(err: &IronControlError, code: u16) -> bool {
    matches!(err, IronControlError::Status { status, .. } if *status == code)
}
