use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::{
    IronProxyConfigError, ProxyConfig, ProxyFragment, Result, Secret, SourcePolicy, Transform,
    load_default_proxy_base_config, resolve_placeholder_source_values, value_field_str,
};

pub fn render_proxy_yaml(base_config: Option<&str>, fragments: &[ProxyFragment]) -> Result<String> {
    render_proxy_yaml_with_source_policy(base_config, fragments, &SourcePolicy::default())
}

pub fn render_proxy_yaml_with_source_policy(
    base_config: Option<&str>,
    fragments: &[ProxyFragment],
    source_policy: &SourcePolicy,
) -> Result<String> {
    let default_base_config;
    let base_config = match base_config {
        Some(base_config) => base_config,
        None => {
            default_base_config = load_default_proxy_base_config()?;
            default_base_config.as_str()
        }
    };
    let mut cfg: ProxyConfig =
        serde_yaml::from_str(base_config).map_err(IronProxyConfigError::ParseBase)?;

    for fragment in fragments {
        for (key, value) in &fragment.top_level {
            let mut value = value.clone();
            resolve_placeholder_source_values(&mut value, source_policy)?;
            cfg.top_level.insert(key.clone(), value);
        }
    }

    let mut transforms = existing_unmanaged_transforms(cfg.transforms);
    let mut managed = fragments
        .iter()
        .flat_map(|fragment| fragment.transforms.iter().cloned())
        .collect::<Vec<_>>();
    assign_secret_ids(&mut managed)?;
    for transform in &mut managed {
        transform.resolve_sources(source_policy)?;
    }
    insert_before_header_allowlist(&mut transforms, managed);
    cfg.transforms = transforms;

    let mut postgres = fragments
        .iter()
        .flat_map(|fragment| fragment.postgres.iter().cloned())
        .collect::<Vec<_>>();
    for listener in &mut postgres {
        listener.resolve_sources(source_policy)?;
    }
    cfg.postgres = postgres;

    serde_yaml::to_string(&cfg).map_err(IronProxyConfigError::Serialize)
}

fn assign_secret_ids(transforms: &mut [Transform]) -> Result<()> {
    let mut used = BTreeMap::<String, usize>::new();
    for secret in transforms
        .iter()
        .filter(|transform| transform.is_secrets())
        .flat_map(|transform| transform.config.secrets.iter())
    {
        if let Some(id) = secret.explicit_id() {
            used.entry(id.to_owned()).or_insert(1);
        }
    }

    for transform in transforms
        .iter_mut()
        .filter(|transform| transform.is_secrets())
    {
        for secret in &mut transform.config.secrets {
            if secret.explicit_id().is_some() {
                continue;
            }
            let candidate = generated_secret_id(secret)?;
            secret.id = Some(unique_id(candidate, &mut used));
        }
    }
    Ok(())
}

fn generated_secret_id(secret: &Secret) -> Result<String> {
    let base = secret_id_base(secret);
    let digest = secret_identity_digest(secret)?;
    Ok(format!("{base}-{digest}"))
}

fn secret_id_base(secret: &Secret) -> String {
    let raw = secret
        .proxy_value()
        .or_else(|| value_field_str(secret.source.as_ref(), "credential_id"))
        .or_else(|| value_field_str(secret.source.as_ref(), "placeholder"))
        .or_else(|| value_field_str(secret.source.as_ref(), "var"))
        .or_else(|| value_field_str(secret.source.as_ref(), "secret_ref"))
        .or_else(|| value_field_str(secret.inject.as_ref(), "header"))
        .or_else(|| value_field_str(secret.inject.as_ref(), "query_param"))
        .or_else(|| {
            secret
                .rules
                .first()
                .and_then(|rule| value_field_str(Some(rule), "host"))
        })
        .unwrap_or("secret");
    let slug = slugify_id_component(raw);
    if slug.is_empty() {
        "secret".to_owned()
    } else {
        slug
    }
}

fn slugify_id_component(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn secret_identity_digest(secret: &Secret) -> Result<String> {
    let mut identity = secret.clone();
    identity.id = None;
    let serialized = serde_yaml::to_string(&identity).map_err(IronProxyConfigError::Serialize)?;
    let digest = Sha256::digest(serialized.as_bytes());
    Ok(digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn unique_id(candidate: String, used: &mut BTreeMap<String, usize>) -> String {
    let count = used.entry(candidate.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        candidate
    } else {
        format!("{candidate}-{count}")
    }
}

fn existing_unmanaged_transforms(transforms: Vec<Transform>) -> Vec<Transform> {
    transforms
        .into_iter()
        .filter(|transform| !transform.is_managed())
        .collect()
}

fn insert_before_header_allowlist(transforms: &mut Vec<Transform>, managed: Vec<Transform>) {
    if let Some(index) = transforms
        .iter()
        .position(|transform| transform.name == "header_allowlist")
    {
        transforms.splice(index..index, managed);
    } else {
        transforms.extend(managed);
    }
}
