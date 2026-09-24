use std::collections::HashMap;

use serde_json::Value;

use crate::models::Credential;

pub(crate) fn suppress(credentials: &mut Vec<Credential>) {
    let mut indexes: Vec<usize> = (0..credentials.len()).collect();
    indexes.sort_by_key(|&i| (-credentials[i].priority, -credentials[i].id));
    let mut claimed: HashMap<String, Vec<(String, i32)>> = HashMap::new();
    let mut suppressed = vec![false; credentials.len()];
    for i in indexes {
        let claims = claims(&credentials[i]);
        let stronger = claims.iter().any(|(scope, target)| {
            claimed.get(target).is_some_and(|prior| {
                prior.iter().any(|(other, priority)| {
                    *priority > credentials[i].priority && scopes_overlap(scope, other)
                })
            })
        });
        if stronger {
            suppressed[i] = true;
        } else {
            for (scope, target) in claims {
                claimed
                    .entry(target)
                    .or_default()
                    .push((scope, credentials[i].priority));
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
    if credential.kind == "pg_dsn" {
        return Vec::new();
    }
    let targets: Vec<String> = match credential.kind.as_str() {
        "static" => {
            if let Some(inject) = credential
                .data
                .get("inject_config")
                .filter(|value| present(Some(value)))
            {
                if let Some(header) = inject.get("header").and_then(Value::as_str) {
                    vec![format!("header:{}", header.to_lowercase())]
                } else if let Some(param) = inject.get("query_param").and_then(Value::as_str) {
                    vec![format!("query:{param}")]
                } else {
                    vec![]
                }
            } else {
                credential
                    .data
                    .get("replace_config")
                    .and_then(|value| value.get("match_headers"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|value| format!("header:{}", value.to_lowercase()))
                    .collect()
            }
        }
        "gcp_auth" | "aws_auth" | "oauth_token" => vec!["header:authorization".to_owned()],
        "gcp_id_token" => vec![format!(
            "header:{}",
            credential
                .data
                .get("header")
                .and_then(Value::as_str)
                .unwrap_or("authorization")
        )],
        "hmac" => credential
            .data
            .get("headers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|header| header.get("name").and_then(Value::as_str))
            .map(|header| format!("header:{}", header.to_lowercase()))
            .collect(),
        _ => vec![],
    };
    let scopes = credential.rules.iter().filter_map(|rule| {
        if let Some(host) = rule.get("host").and_then(Value::as_str) {
            Some(format!(
                "host:{}",
                host.trim().trim_end_matches('.').to_lowercase()
            ))
        } else {
            rule.get("cidr")
                .and_then(Value::as_str)
                .map(|value| format!("cidr:{value}"))
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

fn present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::suppress;
    use crate::models::Credential;

    #[test]
    fn higher_priority_conflict_suppresses_lower_priority() {
        let make = |kind: &str, id, priority| Credential {
            kind: kind.to_owned(),
            id,
            priority,
            data: if kind == "static" {
                json!({"inject_config":{"header":"Authorization"}})
            } else {
                json!({})
            },
            sources: vec![],
            rules: vec![json!({"host":"api.example.com"})],
        };
        let mut credentials = vec![make("gcp_auth", 1, 0), make("static", 2, 100)];
        suppress(&mut credentials);
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].kind, "static");
    }
}
