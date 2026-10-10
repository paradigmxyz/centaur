use std::collections::HashMap;

use crate::models::{Credential, CredentialData};

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

/// The (scope, target) pairs a credential writes: each host or CIDR its rules
/// match crossed with each header or query param it sets. Mirrors what the
/// proxy actually writes, so two credentials conflict only when one would
/// overwrite the other at runtime.
fn claims(credential: &Credential) -> Vec<(String, String)> {
    let targets: Vec<String> = match &credential.data {
        CredentialData::Static(data) => {
            if let Some(inject) = data.inject_config.as_ref().filter(|value| present(value)) {
                if let Some(header) = nonblank(inject.get("header")) {
                    vec![format!("header:{}", header.to_lowercase())]
                } else if let Some(param) = nonblank(inject.get("query_param")) {
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
        CredentialData::GcpAuth(_) | CredentialData::AwsAuth(_) => {
            vec!["header:authorization".to_owned()]
        }
        CredentialData::OauthToken(data) => vec![header_target(data.header.as_deref())],
        CredentialData::GcpIdToken(data) => vec![header_target(data.header.as_deref())],
        CredentialData::Hmac(data) => data
            .headers
            .iter()
            .map(|header| format!("header:{}", header.name.to_lowercase()))
            .collect(),
        CredentialData::PgDsn(_) => vec![],
    };
    let scopes = credential.rules.iter().filter_map(|rule| {
        if let Some(host) = rule.host.as_deref().filter(|host| !host.trim().is_empty()) {
            Some(format!(
                "host:{}",
                host.trim().trim_end_matches('.').to_lowercase()
            ))
        } else {
            rule.cidr
                .as_deref()
                .filter(|cidr| !cidr.trim().is_empty())
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

/// A transform's configured header, defaulting to Authorization like the proxy.
fn header_target(header: Option<&str>) -> String {
    let header = header
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .unwrap_or("authorization");
    format!("header:{}", header.to_lowercase())
}

fn nonblank(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
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
    use serde_json::{Value, json};

    use super::suppress;
    use crate::models::{
        AwsAuthData, Credential, CredentialData, CredentialKind, GcpAuthData, GcpIdTokenData,
        HmacData, HmacHeader, OauthTokenData, PgDsnData, RequestRule, StaticData,
    };

    // Direct grants default to priority 100 and role grants to 0 in Console.
    const DIRECT: i32 = 100;
    const ROLE: i32 = 0;

    fn rules(hosts: &[&str]) -> Vec<RequestRule> {
        hosts
            .iter()
            .map(|host| RequestRule {
                host: Some((*host).to_owned()),
                cidr: None,
                http_methods: vec![],
                paths: vec![],
            })
            .collect()
    }

    fn credential(
        kind: CredentialKind,
        id: i64,
        priority: i32,
        data: CredentialData,
        hosts: &[&str],
    ) -> Credential {
        Credential {
            kind,
            id,
            priority,
            data,
            sources: vec![],
            rules: rules(hosts),
        }
    }

    fn static_with(
        id: i64,
        priority: i32,
        inject: Option<Value>,
        replace: Option<Value>,
        hosts: &[&str],
    ) -> Credential {
        credential(
            CredentialKind::Static,
            id,
            priority,
            CredentialData::Static(StaticData {
                inject_config: inject,
                replace_config: replace,
            }),
            hosts,
        )
    }

    fn inject(id: i64, priority: i32, header: &str, hosts: &[&str]) -> Credential {
        static_with(id, priority, Some(json!({ "header": header })), None, hosts)
    }

    fn gcp(id: i64, priority: i32, hosts: &[&str]) -> Credential {
        credential(
            CredentialKind::GcpAuth,
            id,
            priority,
            CredentialData::GcpAuth(GcpAuthData {
                credentials_provider: None,
                subject: None,
                scopes: vec![],
            }),
            hosts,
        )
    }

    fn oauth(id: i64, priority: i32, header: Option<&str>, hosts: &[&str]) -> Credential {
        credential(
            CredentialKind::OauthToken,
            id,
            priority,
            CredentialData::OauthToken(OauthTokenData {
                grant: "refresh_token".to_owned(),
                token_endpoint: "https://oauth2.googleapis.com/token".to_owned(),
                audience: None,
                scopes: vec![],
                header: header.map(str::to_owned),
                value_prefix: None,
            }),
            hosts,
        )
    }

    fn served(mut credentials: Vec<Credential>) -> Vec<(CredentialKind, i64)> {
        suppress(&mut credentials);
        credentials
            .iter()
            .map(|credential| (credential.kind, credential.id))
            .collect()
    }

    #[test]
    fn higher_priority_conflict_suppresses_lower_priority() {
        assert_eq!(
            served(vec![
                gcp(1, ROLE, &["api.example.com"]),
                inject(2, DIRECT, "Authorization", &["api.example.com"]),
            ]),
            vec![(CredentialKind::Static, 2)]
        );
    }

    #[test]
    fn promoted_role_transform_suppresses_lower_direct_static() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["api.example.com"]),
                gcp(2, 900, &["api.example.com"]),
            ]),
            vec![(CredentialKind::GcpAuth, 2)]
        );
    }

    #[test]
    fn different_headers_on_the_same_host_both_serve() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "X-Api-Key", &["api.example.com"]),
                gcp(2, ROLE, &["api.example.com"]),
            ])
            .len(),
            2
        );
    }

    #[test]
    fn same_header_on_different_hosts_both_serve() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["api.example.com"]),
                gcp(2, ROLE, &["other.example.com"]),
            ])
            .len(),
            2
        );
    }

    #[test]
    fn equal_priority_conflicts_are_left_to_the_proxy() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["api.example.com"]),
                gcp(2, DIRECT, &["api.example.com"]),
            ])
            .len(),
            2
        );
    }

    #[test]
    fn header_and_host_matching_ignore_case_and_trailing_dots() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "AUTHORIZATION", &["API.Example.com."]),
                gcp(2, ROLE, &["api.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1)]
        );
    }

    #[test]
    fn wildcard_hosts_conflict_with_matching_exact_hosts_only() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["*.googleapis.com"]),
                gcp(2, ROLE, &["bigquery.googleapis.com"]),
                oauth(3, ROLE, None, &["gmail.googleapis.com"]),
                gcp(4, ROLE, &["googleapis.com"]),
                gcp(5, ROLE, &["a.b.googleapis.com"]),
            ]),
            vec![
                (CredentialKind::Static, 1),
                (CredentialKind::GcpAuth, 4),
                (CredentialKind::GcpAuth, 5),
            ]
        );
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["*"]),
                gcp(2, ROLE, &["anything.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1)]
        );
    }

    #[test]
    fn oauth_tokens_claim_their_configured_header() {
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["api.example.com"]),
                oauth(2, ROLE, Some("X-Goog-Api-Token"), &["api.example.com"]),
            ])
            .len(),
            2
        );
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "x-goog-api-token", &["api.example.com"]),
                oauth(2, ROLE, Some("X-Goog-Api-Token"), &["api.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1)]
        );
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["api.example.com"]),
                oauth(2, ROLE, Some(""), &["api.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1)]
        );
    }

    #[test]
    fn gcp_id_tokens_claim_their_configured_header() {
        let id_token = |id, header: Option<&str>| {
            credential(
                CredentialKind::GcpIdToken,
                id,
                ROLE,
                CredentialData::GcpIdToken(GcpIdTokenData {
                    audience: "https://run.example".to_owned(),
                    header: header.map(str::to_owned),
                }),
                &["run.example.com"],
            )
        };
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["run.example.com"]),
                id_token(2, Some("x-serverless-authorization")),
                id_token(3, None),
            ]),
            vec![(CredentialKind::Static, 1), (CredentialKind::GcpIdToken, 2)]
        );
    }

    #[test]
    fn aws_and_hmac_claim_their_headers() {
        let aws = credential(
            CredentialKind::AwsAuth,
            2,
            ROLE,
            CredentialData::AwsAuth(AwsAuthData {
                allowed_regions: vec![],
                allowed_services: vec![],
            }),
            &["logs.amazonaws.com"],
        );
        let hmac = credential(
            CredentialKind::Hmac,
            3,
            ROLE,
            CredentialData::Hmac(HmacData {
                timestamp_format: "unix".to_owned(),
                signature_algorithm: "sha256".to_owned(),
                signature_key_encoding: "raw".to_owned(),
                signature_output_encoding: "hex".to_owned(),
                signature_message: "{{ .Body }}".to_owned(),
                headers: vec![HmacHeader {
                    name: "X-Signature".to_owned(),
                    value: "{{ .Signature }}".to_owned(),
                }],
                allow_chunked_body: false,
            }),
            &["hooks.example.com"],
        );
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &["logs.amazonaws.com"]),
                aws,
                hmac,
                inject(4, DIRECT, "x-signature", &["hooks.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1), (CredentialKind::Static, 4)]
        );
    }

    #[test]
    fn replace_secrets_claim_their_match_headers() {
        let bot_token = static_with(
            1,
            ROLE,
            None,
            Some(json!({ "proxy_value": "SLACK_BOT_TOKEN", "match_headers": ["Authorization"] })),
            &["slack.com"],
        );
        let body_replace = static_with(
            2,
            ROLE,
            None,
            Some(json!({ "proxy_value": "BODY_TOKEN" })),
            &["slack.com"],
        );
        assert_eq!(
            served(vec![
                bot_token,
                body_replace,
                inject(3, DIRECT, "Authorization", &["slack.com"]),
            ]),
            vec![(CredentialKind::Static, 2), (CredentialKind::Static, 3)]
        );
    }

    #[test]
    fn query_params_and_cidrs_are_matched_exactly() {
        let query = |id, priority, param: &str| {
            static_with(
                id,
                priority,
                Some(json!({ "query_param": param })),
                None,
                &["api.example.com"],
            )
        };
        assert_eq!(
            served(vec![
                query(1, DIRECT, "key"),
                query(2, ROLE, "key"),
                query(3, ROLE, "KEY")
            ]),
            vec![(CredentialKind::Static, 1), (CredentialKind::Static, 3)]
        );

        let cidr = |id, priority, cidr: &str| {
            let mut credential = inject(id, priority, "Authorization", &[]);
            credential.rules = vec![RequestRule {
                host: None,
                cidr: Some(cidr.to_owned()),
                http_methods: vec![],
                paths: vec![],
            }];
            credential
        };
        assert_eq!(
            served(vec![
                cidr(1, DIRECT, "10.0.0.0/8"),
                cidr(2, ROLE, "10.0.0.0/8"),
                cidr(3, ROLE, "10.1.0.0/16"),
            ]),
            vec![(CredentialKind::Static, 1), (CredentialKind::Static, 3)]
        );
    }

    #[test]
    fn suppressed_credentials_do_not_claim_their_other_scopes() {
        assert_eq!(
            served(vec![
                inject(1, 200, "Authorization", &["a.example.com"]),
                gcp(2, DIRECT, &["a.example.com", "b.example.com"]),
                inject(3, ROLE, "Authorization", &["b.example.com"]),
            ]),
            vec![(CredentialKind::Static, 1), (CredentialKind::Static, 3)]
        );
    }

    #[test]
    fn credentials_without_scopes_or_targets_never_conflict() {
        let postgres = credential(
            CredentialKind::PgDsn,
            2,
            ROLE,
            CredentialData::PgDsn(PgDsnData {
                foreign_id: "analytics".to_owned(),
                database: "analytics".to_owned(),
                role: None,
                settings: vec![],
            }),
            &[],
        );
        assert_eq!(
            served(vec![
                inject(1, DIRECT, "Authorization", &[]),
                postgres,
                static_with(
                    3,
                    ROLE,
                    Some(json!({ "header": "" })),
                    None,
                    &["api.example.com"]
                ),
                gcp(4, ROLE, &["api.example.com"]),
            ])
            .len(),
            4
        );
    }
}
