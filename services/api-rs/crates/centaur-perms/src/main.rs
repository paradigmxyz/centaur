//! `centaur-perms` — manage iron-control permissions for Centaur: which Slack
//! principals (users / channels) and roles hold which tool roles and secrets.
//!
//! Commands are resource-first: `centaur-perms <noun> <verb>`, where the noun is
//! `principals` or `roles`. The CLI reuses `centaur-iron-control`'s canonical
//! mappings (`derive_principal`, `RoleSpec::tool`) so the principal and role
//! `foreign_id`s it writes match exactly what api-rs registers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use centaur_iron_control::{
    GrantSecret, Grantee, IdentityInput, IronControlClient, IronControlError, Role, RoleSpec,
    grant_inputs_to_role,
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
    /// Slack thread key (`slack:T…:C…[:ts]`, derived), a principal `foreign_id`
    /// (e.g. `slack-channel-t1-c9`), or an OID (`prn_…`).
    principal: String,

    /// Acting Slack user id, used only to key a DM principal from a thread key.
    #[arg(long)]
    slack_user: Option<String>,
}

#[derive(Args, Debug)]
struct PrincipalGrantArgs {
    /// Slack thread key (derived) or raw principal `foreign_id`.
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

    /// Secret OID (`ssr_`/`ots_`/`gas_`) to grant/revoke directly. Repeatable.
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

    /// Secret OID (`ssr_`/`ots_`/`gas_`) to grant/revoke. Repeatable.
    #[arg(long = "secret", value_name = "OID", required = true)]
    secrets: Vec<String>,
}

#[derive(Args, Debug)]
struct RoleGrantArgs {
    /// Role `foreign_id` (e.g. `infra`, `tools`, `tool-github`) or OID.
    role: String,

    /// Existing secret OID (`ssr_`/`ots_`/`gas_`) to grant. Repeatable.
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
            RolesCmd::Show(args) => roles_show(&client, args).await,
            RolesCmd::Grant(args) => roles_grant(&cli, &client, args).await,
            RolesCmd::Revoke(args) => roles_revoke(&client, args).await,
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
        found.iter().map(|p| (p.foreign_id.as_deref(), p.id.as_str(), p.name.as_str())),
        &cli.namespace,
        "principal",
    );
    Ok(())
}

async fn principals_show(cli: &Cli, client: &IronControlClient, args: &PrincipalSelector) -> Result<()> {
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &identity.foreign_id).await?;
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
            println!("  {} ({})", role.foreign_id.as_deref().unwrap_or("-"), role.id);
            for grant in client.list_role_grants(&role.id).await? {
                if let Some(secret) = grant.secret_id() {
                    println!("    grants {secret}");
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
            if let Some(secret) = grant.secret_id() {
                println!("  {secret} (grant {})", grant.id);
            }
        }
    }

    let effective = client.effective_config(&principal.id).await?;
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

async fn principals_grant(cli: &Cli, client: &IronControlClient, args: &PrincipalGrantArgs) -> Result<()> {
    if args.tools.is_empty() && args.roles.is_empty() && args.secrets.is_empty() {
        bail!("nothing to grant: pass at least one --tool, --role, or --secret");
    }
    if !args.grant_ids.is_empty() {
        bail!("--grant-id is only valid for `principals revoke`");
    }
    let policy = build_source_policy(cli)?;
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal_id = ensure_principal(client, &identity).await?;
    println!("principal: {} ({principal_id})", identity.foreign_id);

    let dirs = tools::resolve_tool_dirs(&cli.tools_dirs, std::env::var("TOOL_DIRS").ok().as_deref());
    for tool in &args.tools {
        let manifest = tools::find_tool(&dirs, tool)?;
        let role = RoleSpec::tool(&manifest.name);
        let role_id = client.upsert_role(&role_identity(&role, &cli.namespace)).await?.id;
        let secrets: Vec<_> = manifest.all_secrets().cloned().collect();
        let translation = translate::translate(&cli.namespace, &role.foreign_id, &secrets, &policy);
        let granted = grant_inputs_to_role(client, &role_id, translation.inputs).await?;
        assign_role_idempotent(client, &principal_id, &role_id).await?;
        println!(
            "  tool {} (from {}): role {} ({role_id}) — {} secret(s) registered, role assigned",
            manifest.name,
            manifest.dir.display(),
            role.foreign_id,
            granted.len()
        );
        for (name, kind) in &translation.skipped {
            println!("    skipped {name} (unsupported secret type {kind:?})");
        }
    }

    for role_fid in &args.roles {
        let role = get_role_or_fail(client, role_fid).await?;
        assign_role_idempotent(client, &principal_id, &role.id).await?;
        println!("  role {role_fid} ({}): assigned", role.id);
    }

    for oid in &args.secrets {
        let secret = grant_secret_from_oid(oid)?;
        let grant = client.create_grant(&Grantee::Principal(principal_id.clone()), &secret).await?;
        println!("  secret {oid}: granted ({})", grant.id);
    }
    Ok(())
}

async fn principals_revoke(cli: &Cli, client: &IronControlClient, args: &PrincipalGrantArgs) -> Result<()> {
    if args.tools.is_empty() && args.roles.is_empty() && args.secrets.is_empty() && args.grant_ids.is_empty() {
        bail!("nothing to revoke: pass at least one --tool, --role, --secret, or --grant-id");
    }
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &identity.foreign_id).await?;
    println!("principal: {} ({})", identity.foreign_id, principal.id);

    let assigned = client.list_principal_roles(&principal.id).await?;
    let role_targets = args
        .tools
        .iter()
        .map(|t| (t.as_str(), RoleSpec::tool(t).foreign_id))
        .chain(args.roles.iter().map(|r| (r.as_str(), r.clone())));
    for (label, role_fid) in role_targets {
        match assigned.iter().find(|r| r.foreign_id.as_deref() == Some(role_fid.as_str())) {
            Some(role) => {
                client.unassign_role(&principal.id, &role.id).await?;
                println!("  {label}: role {role_fid} unassigned");
            }
            None => println!("  {label}: role {role_fid} was not assigned — nothing to do"),
        }
    }

    if !args.secrets.is_empty() {
        let grants = client.list_principal_grants(&principal.id).await?;
        for oid in &args.secrets {
            match grants.iter().find(|g| g.secret_id() == Some(oid.as_str())) {
                Some(grant) => {
                    client.delete_grant(&grant.id).await?;
                    println!("  secret {oid}: grant {} revoked", grant.id);
                }
                None => println!("  secret {oid}: no direct grant on this principal — nothing to do"),
            }
        }
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
        found.iter().map(|r| (r.foreign_id.as_deref(), r.id.as_str(), r.name.as_str())),
        &cli.namespace,
        "role",
    );
    Ok(())
}

async fn roles_show(client: &IronControlClient, args: &RoleSelector) -> Result<()> {
    let role = get_role_or_fail(client, &args.role).await?;
    println!("role: {} ({}) — {}", role.foreign_id.as_deref().unwrap_or("-"), role.id, role.name);
    let grants = client.list_role_grants(&role.id).await?;
    if grants.is_empty() {
        println!("secrets: (none)");
    } else {
        println!("secrets:");
        for grant in &grants {
            if let Some(secret) = grant.secret_id() {
                println!("  {secret} (grant {})", grant.id);
            }
        }
    }
    Ok(())
}

async fn roles_grant(cli: &Cli, client: &IronControlClient, args: &RoleGrantArgs) -> Result<()> {
    if args.secrets.is_empty() && args.tool.is_none() {
        bail!("nothing to grant: pass at least one --secret <OID> or --tool <NAME>");
    }
    let role = get_role_or_fail(client, &args.role).await?;
    println!("role: {} ({})", role.foreign_id.as_deref().unwrap_or("-"), role.id);

    for oid in &args.secrets {
        let secret = grant_secret_from_oid(oid)?;
        let grant = client.create_grant(&Grantee::Role(role.id.clone()), &secret).await?;
        println!("  secret {oid}: granted ({})", grant.id);
    }

    if let Some(tool) = &args.tool {
        let policy = build_source_policy(cli)?;
        let dirs = tools::resolve_tool_dirs(&cli.tools_dirs, std::env::var("TOOL_DIRS").ok().as_deref());
        let manifest = tools::find_tool(&dirs, tool)?;
        let selected = select_secrets(manifest.all_secrets().cloned().collect(), &args.secret_names)?;
        // Key the secret resources on the tool's canonical role so the same
        // secret object is shared no matter which role it's granted to.
        let tool_role = RoleSpec::tool(&manifest.name).foreign_id;
        let translation = translate::translate(&cli.namespace, &tool_role, &selected, &policy);
        let granted = grant_inputs_to_role(client, &role.id, translation.inputs).await?;
        println!(
            "  tool {} (from {}): {} secret(s) registered and granted to {}",
            manifest.name,
            manifest.dir.display(),
            granted.len(),
            role.foreign_id.as_deref().unwrap_or(&role.id)
        );
        for (name, kind) in &translation.skipped {
            println!("    skipped {name} (unsupported secret type {kind:?})");
        }
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

async fn roles_revoke(client: &IronControlClient, args: &RoleSecretArgs) -> Result<()> {
    let role = get_role_or_fail(client, &args.role).await?;
    println!("role: {} ({})", role.foreign_id.as_deref().unwrap_or("-"), role.id);
    let grants = client.list_role_grants(&role.id).await?;
    for oid in &args.secrets {
        match grants.iter().find(|g| g.secret_id() == Some(oid.as_str())) {
            Some(grant) => {
                client.delete_grant(&grant.id).await?;
                println!("  secret {oid}: grant {} revoked", grant.id);
            }
            None => println!("  secret {oid}: not granted to this role — nothing to do"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn filter_labels(args: &FilterArgs) -> Result<Vec<(String, String)>> {
    let mut labels = args.labels.iter().map(|l| parse_label(l)).collect::<Result<Vec<_>>>()?;
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
    let width = rows.iter().map(|(fid, _, _)| fid.unwrap_or("-").len()).max().unwrap_or(0);
    for (fid, id, name) in &rows {
        println!("{:<width$}  {}  {}", fid.unwrap_or("-"), id, name, width = width);
    }
    println!("({} {noun}(s))", rows.len());
}

fn build_source_policy(cli: &Cli) -> Result<SourcePolicy> {
    Ok(match cli.source_policy {
        SourcePolicyArg::Env => SourcePolicy::env(),
        SourcePolicyArg::Onepassword | SourcePolicyArg::OnepasswordConnect => {
            let vault = cli
                .op_vault
                .clone()
                .ok_or_else(|| eyre::eyre!("--op-vault is required for --source-policy onepassword*"))?;
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
        labels: BTreeMap::from([("managed-by".to_owned(), "centaur".to_owned())]),
    }
}

fn grant_secret_from_oid(oid: &str) -> Result<GrantSecret> {
    if oid.starts_with("ssr_") {
        Ok(GrantSecret::Static(oid.to_owned()))
    } else if oid.starts_with("ots_") {
        Ok(GrantSecret::OAuthToken(oid.to_owned()))
    } else if oid.starts_with("gas_") {
        Ok(GrantSecret::GcpAuth(oid.to_owned()))
    } else {
        bail!("--secret expects a secret OID (ssr_/ots_/gas_), got {oid:?}");
    }
}

/// Parse a `key=value` label filter.
fn parse_label(raw: &str) -> Result<(String, String)> {
    match raw.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_owned(), v.to_owned())),
        _ => bail!("--label must be key=value, got {raw:?}"),
    }
}

/// Ensure the principal exists, returning its OID. Looks it up first so an
/// existing principal (e.g. one a session created) is never clobbered; creates
/// it only when absent.
async fn ensure_principal(client: &IronControlClient, identity: &IdentityInput) -> Result<String> {
    match client.get_principal(&identity.foreign_id).await {
        Ok(p) => Ok(p.id),
        Err(e) if is_status(&e, 404) => Ok(client.upsert_principal(identity).await?.id),
        Err(e) => Err(e.into()),
    }
}

async fn get_principal_or_fail(
    client: &IronControlClient,
    foreign_id: &str,
) -> Result<centaur_iron_control::Principal> {
    match client.get_principal(foreign_id).await {
        Ok(p) => Ok(p),
        Err(e) if is_status(&e, 404) => bail!("principal {foreign_id:?} not found in iron-control"),
        Err(e) => Err(e.into()),
    }
}

async fn get_role_or_fail(client: &IronControlClient, role: &str) -> Result<Role> {
    match client.get_role(role).await {
        Ok(r) => Ok(r),
        Err(e) if is_status(&e, 404) => bail!("role {role:?} not found in iron-control"),
        Err(e) => Err(e.into()),
    }
}

/// Assign the role, treating an already-assigned conflict as success.
async fn assign_role_idempotent(client: &IronControlClient, principal_id: &str, role_id: &str) -> Result<()> {
    match client.assign_role(principal_id, role_id).await {
        Ok(()) => Ok(()),
        Err(e) if is_status(&e, 409) || is_status(&e, 422) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn is_status(err: &IronControlError, code: u16) -> bool {
    matches!(err, IronControlError::Status { status, .. } if *status == code)
}
