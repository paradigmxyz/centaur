//! `centaur-grants` — grant Slack principals (users / channels) access to a
//! tool's role (and its secrets) or to an individual secret in iron-control.
//!
//! The CLI reuses `centaur-iron-control`'s canonical mappings (`derive_principal`,
//! `RoleSpec::tool`) so the principal and role `foreign_id`s it writes match
//! exactly what api-rs registers — a grant the CLI adds is the same grant a
//! running session resolves.

use std::collections::BTreeMap;
use std::path::PathBuf;

use centaur_iron_control::{
    GrantSecret, Grantee, IdentityInput, IronControlClient, IronControlError, RoleSpec,
    grant_inputs_to_role,
};
use centaur_iron_proxy::SourcePolicy;
use clap::{Args, Parser, Subcommand, ValueEnum};
use eyre::{Result, bail};

mod principal;
mod tools;
mod translate;

#[cfg(test)]
mod tests;

#[derive(Parser, Debug)]
#[command(
    name = "centaur-grants",
    about = "Grant Slack principals access to tool roles and secrets in iron-control"
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
    /// Grant a principal access to one or more tools (role + secrets) or secrets.
    Add(GrantArgs),
    /// Revoke a principal's access to one or more tools or secret grants.
    Revoke(GrantArgs),
    /// List the roles and effective secrets a principal resolves to.
    List(ListArgs),
    /// List principals registered in iron-control (for discovery).
    Principals(PrincipalsArgs),
}

#[derive(Args, Debug)]
struct GrantArgs {
    /// Principal: a Slack thread key (`slack:T…:C…[:ts]`, derived) or a raw
    /// principal `foreign_id` (e.g. `slack-channel-t1-c9`).
    #[arg(long)]
    principal: String,

    /// Acting Slack user id, used only to key a DM principal from a thread key.
    #[arg(long)]
    slack_user: Option<String>,

    /// Tool name to grant/revoke (its `tool-{slug}` role). Repeatable.
    #[arg(long = "tool", value_name = "NAME")]
    tools: Vec<String>,

    /// Secret OID (`ssr_`/`ots_`/`gas_`) to grant (`add`) or revoke (`revoke`,
    /// by finding the principal's matching grant). Repeatable.
    #[arg(long = "secret", value_name = "OID")]
    secrets: Vec<String>,

    /// Grant OID (`grant_…`) to revoke directly. `revoke` only. Repeatable.
    #[arg(long = "grant-id", value_name = "OID")]
    grant_ids: Vec<String>,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Principal: a Slack thread key (derived) or a raw principal `foreign_id`.
    #[arg(long)]
    principal: String,

    /// Acting Slack user id, used only to key a DM principal from a thread key.
    #[arg(long)]
    slack_user: Option<String>,
}

#[derive(Args, Debug)]
struct PrincipalsArgs {
    /// Only principals carrying this label. Repeatable: `--label key=value`.
    #[arg(long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,

    /// Case-insensitive substring to match against `foreign_id` or name.
    #[arg(long)]
    filter: Option<String>,

    /// Only show Centaur-managed principals (label `managed-by=centaur`).
    #[arg(long)]
    managed: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = IronControlClient::new(&cli.iron_control_url, &cli.iron_control_api_key);
    let policy = build_source_policy(&cli)?;

    match &cli.command {
        Command::Add(args) => add(&cli, &client, &policy, args).await,
        Command::Revoke(args) => revoke(&cli, &client, args).await,
        Command::List(args) => list(&cli, &client, args).await,
        Command::Principals(args) => principals(&cli, &client, args).await,
    }
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

async fn add(cli: &Cli, client: &IronControlClient, policy: &SourcePolicy, args: &GrantArgs) -> Result<()> {
    if args.tools.is_empty() && args.secrets.is_empty() {
        bail!("nothing to grant: pass at least one --tool or --secret");
    }
    if !args.grant_ids.is_empty() {
        bail!("--grant-id is only valid for `revoke`");
    }
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal_id = ensure_principal(client, &identity).await?;
    println!("principal: {} ({principal_id})", identity.foreign_id);

    let dirs = tools::resolve_tool_dirs(&cli.tools_dirs, std::env::var("TOOL_DIRS").ok().as_deref());
    for tool in &args.tools {
        let manifest = tools::find_tool(&dirs, tool)?;
        let role = RoleSpec::tool(&manifest.name);
        let role_id = client.upsert_role(&role_identity(&role, &cli.namespace)).await?.id;

        let secrets: Vec<_> = manifest.all_secrets().cloned().collect();
        let translation = translate::translate(&cli.namespace, &role.foreign_id, &secrets, policy);
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

    for oid in &args.secrets {
        let secret = grant_secret_from_oid(oid)?;
        let grant = client.create_grant(&Grantee::Principal(principal_id.clone()), &secret).await?;
        println!("  secret {oid}: granted ({})", grant.id);
    }
    Ok(())
}

async fn revoke(cli: &Cli, client: &IronControlClient, args: &GrantArgs) -> Result<()> {
    if args.tools.is_empty() && args.secrets.is_empty() && args.grant_ids.is_empty() {
        bail!("nothing to revoke: pass at least one --tool, --secret, or --grant-id");
    }
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &identity.foreign_id).await?;
    println!("principal: {} ({})", identity.foreign_id, principal.id);

    let assigned = client.list_principal_roles(&principal.id).await?;
    for tool in &args.tools {
        let role_fid = RoleSpec::tool(tool).foreign_id;
        match assigned.iter().find(|r| r.foreign_id.as_deref() == Some(role_fid.as_str())) {
            Some(role) => {
                client.unassign_role(&principal.id, &role.id).await?;
                println!("  tool {tool}: role {role_fid} unassigned");
            }
            None => println!("  tool {tool}: role {role_fid} was not assigned — nothing to do"),
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

async fn list(cli: &Cli, client: &IronControlClient, args: &ListArgs) -> Result<()> {
    let identity = principal::resolve_principal(&args.principal, args.slack_user.as_deref(), &cli.namespace);
    let principal = get_principal_or_fail(client, &identity.foreign_id).await?;
    println!("principal: {} ({}) — {}", identity.foreign_id, principal.id, principal.name);

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

async fn principals(cli: &Cli, client: &IronControlClient, args: &PrincipalsArgs) -> Result<()> {
    let mut labels = args.labels.iter().map(|l| parse_label(l)).collect::<Result<Vec<_>>>()?;
    if args.managed {
        labels.push(("managed-by".to_owned(), "centaur".to_owned()));
    }

    let mut found = client.list_principals(&cli.namespace, &labels).await?;
    if let Some(needle) = args.filter.as_deref().map(str::to_lowercase) {
        found.retain(|p| {
            p.foreign_id.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                || p.name.to_lowercase().contains(&needle)
        });
    }
    found.sort_by(|a, b| a.foreign_id.cmp(&b.foreign_id));

    if found.is_empty() {
        println!("no principals found in namespace {:?}", cli.namespace);
        return Ok(());
    }
    let width = found
        .iter()
        .map(|p| p.foreign_id.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0);
    for p in &found {
        println!(
            "{:<width$}  {}  {}",
            p.foreign_id.as_deref().unwrap_or("-"),
            p.id,
            p.name,
            width = width
        );
    }
    println!("({} principal(s))", found.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Parse a `key=value` label filter.
fn parse_label(raw: &str) -> Result<(String, String)> {
    match raw.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_owned(), v.to_owned())),
        _ => bail!("--label must be key=value, got {raw:?}"),
    }
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
