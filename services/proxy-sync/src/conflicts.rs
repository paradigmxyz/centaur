use std::collections::HashMap;

use crate::models::{Credential, CredentialData, CredentialKind};

pub(crate) fn suppress(credentials: &mut Vec<Credential>) {
    let mut indexes: Vec<usize> = (0..credentials.len()).collect();
    indexes.sort_by_key(|&index| (-credentials[index].priority, -credentials[index].id));
    let mut claimed: HashMap<String, Vec<(String, i32)>> = HashMap::new();
    let mut suppressed = vec![false; credentials.len()];
    for index in indexes {
        let claims = claims(&credentials[index]);
        let stronger = claims.iter().any(|(scope, target)| {
            claimed.get(target).is_some_and(|prior| {
                prior.iter().any(|(other, priority)| {
                    *priority > credentials[index].priority && scopes_overlap(scope, other)
                })
            })
        });
        if stronger {
            suppressed[index] = true;
        } else {
            for (scope, target) in claims {
                claimed
                    .entry(target)
                    .or_default()
                    .push((scope, credentials[index].priority));
            }
        }
    }
    let mut index = 0;
    credentials.retain(|_| {
        let keep = !suppressed[index];
        index += 1;
        keep
    });
}

fn claims(credential: &Credential) -> Vec<(String, String)> {
    if credential.kind == CredentialKind::PgDsn {
        return Vec::new();
    }
    let targets: Vec<String> = match &credential.data {
        CredentialData::Static(data) => {
            if let Some(inject) = data.inject_config.as_ref().filter(|value| present(value)) {
                if let Some(header) = inject.get("header").and_then(|value| value.as_str()) {
                    vec![format!("header:{}", header.to_lowercase())]
                } else if let Some(param) =
                    inject.get("query_param").and_then(|value| value.as_str())
                {
                    vec![format!("query:{param}")]
                } else {
                    vec![]
                }
            } else {
                data.replace_config
                    .as_ref()
                    .and_then(|value| value.get("match_headers"))
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str())
                    .map(|value| format!("header:{}", value.to_lowercase()))
                    .collect()
            }
        }
        CredentialData::GcpAuth(_) | CredentialData::AwsAuth(_) | CredentialData::OauthToken(_) => {
            vec!["header:authorization".to_owned()]
        }
        CredentialData::GcpIdToken(data) => vec![format!(
            "header:{}",
            data.header.as_deref().unwrap_or("authorization")
        )],
        CredentialData::Hmac(data) => data
            .headers
            .iter()
            .map(|header| format!("header:{}", header.name.to_lowercase()))
            .collect(),
        CredentialData::PgDsn(_) => vec![],
    };
    let scopes = credential.rules.iter().filter_map(|rule| {
        if let Some(host) = &rule.host {
            Some(format!(
                "host:{}",
                host.trim().trim_end_matches('.').to_lowercase()
            ))
        } else {
            rule.cidr.as_ref().map(|value| format!("cidr:{value}"))
        }
    });
    scopes
        .flat_map(|scope| {
            targets
                .iter()
                .cloned()
                .map(move |target| (scope.clone(), target))
        })
        .collect()
}

fn scopes_overlap(a: &str, b: &str) -> bool {
    let Some((kind_a, value_a)) = a.split_once(':') else {
        return false;
    };
    let Some((kind_b, value_b)) = b.split_once(':') else {
        return false;
    };
    if kind_a != kind_b {
        return false;
    }
    if kind_a == "cidr" {
        return value_a == value_b;
    }
    if value_a == value_b || value_a == "*" || value_b == "*" {
        return true;
    }
    let a: Vec<_> = value_a.split('.').collect();
    let b: Vec<_> = value_b.split('.').collect();
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| *x == "*" || y == "*" || *x == y)
}

fn present(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::suppress;
    use crate::models::{
        Credential, CredentialData, CredentialKind, GcpAuthData, RequestRule, StaticData,
    };

    #[test]
    fn higher_priority_conflict_suppresses_lower_priority() {
        let rules = || {
            vec![RequestRule {
                host: Some("api.example.com".to_owned()),
                cidr: None,
                http_methods: vec![],
                paths: vec![],
            }]
        };
        let mut credentials = vec![
            Credential {
                kind: CredentialKind::GcpAuth,
                id: 1,
                priority: 0,
                data: CredentialData::GcpAuth(GcpAuthData {
                    credentials_provider: None,
                    subject: None,
                    scopes: vec![],
                }),
                sources: vec![],
                rules: rules(),
            },
            Credential {
                kind: CredentialKind::Static,
                id: 2,
                priority: 100,
                data: CredentialData::Static(StaticData {
                    inject_config: Some(json!({"header":"Authorization"})),
                    replace_config: None,
                }),
                sources: vec![],
                rules: rules(),
            },
        ];
        suppress(&mut credentials);
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].kind, CredentialKind::Static);
    }
}
