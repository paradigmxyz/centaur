use std::collections::HashMap;

use crate::models::{Credential, CredentialData};

/// Withholds every credential that a strictly higher-priority credential
/// would overwrite: same header or query param on an overlapping host or CIDR.
/// Credentials are claimed strongest first, and a withheld credential claims
/// nothing, so it cannot withhold others.
pub(crate) fn suppress(credentials: &mut Vec<Credential>) {
    // Equal priorities never withhold each other, so their order is irrelevant.
    let mut indexes: Vec<usize> = (0..credentials.len()).collect();
    indexes.sort_by_key(|&index| std::cmp::Reverse(credentials[index].priority));
    let mut claimed: HashMap<String, Claimed> = HashMap::new();
    let mut suppressed = vec![false; credentials.len()];
    for index in indexes {
        let priority = credentials[index].priority;
        let claims = claims(&credentials[index]);
        if claims.iter().any(|(scope, target)| {
            claimed
                .get(target)
                .is_some_and(|claimed| claimed.stronger(scope, priority))
        }) {
            suppressed[index] = true;
        } else {
            for (scope, target) in claims {
                claimed.entry(target).or_default().insert(scope, priority);
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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Scope {
    Host(String),
    Cidr(String),
}

/// The claims accepted for one target. Exact hosts and CIDRs are looked up by
/// value; wildcard hosts, which are rare, are scanned. Claims arrive strongest
/// first, so the first priority recorded for a scope is its highest.
#[derive(Default)]
struct Claimed {
    exact: HashMap<Scope, i32>,
    wildcards: Vec<(String, i32)>,
}

impl Claimed {
    fn stronger(&self, scope: &Scope, priority: i32) -> bool {
        let above = |claimed: &i32| *claimed > priority;
        let overlapping_wildcard = |host: &str| {
            self.wildcards
                .iter()
                .any(|(pattern, claimed)| above(claimed) && hosts_overlap(pattern, host))
        };
        match scope {
            Scope::Host(host) if is_wildcard(host) => {
                overlapping_wildcard(host)
                    || self.exact.iter().any(|(other, claimed)| {
                        above(claimed)
                            && matches!(other, Scope::Host(exact) if hosts_overlap(host, exact))
                    })
            }
            Scope::Host(host) => {
                self.exact.get(scope).is_some_and(above) || overlapping_wildcard(host)
            }
            Scope::Cidr(_) => self.exact.get(scope).is_some_and(above),
        }
    }

    fn insert(&mut self, scope: Scope, priority: i32) {
        match scope {
            Scope::Host(host) if is_wildcard(&host) => self.wildcards.push((host, priority)),
            scope => {
                self.exact.entry(scope).or_insert(priority);
            }
        }
    }
}

fn is_wildcard(host: &str) -> bool {
    host.split('.').any(|label| label == "*")
}

fn label_count(host: &str) -> usize {
    host.split('.').count()
}

/// Host patterns overlap when they are equal, either is `*`, or they have the
/// same number of labels and each label pair matches or one side is `*`.
fn hosts_overlap(a: &str, b: &str) -> bool {
    if a == b || a == "*" || b == "*" {
        return true;
    }
    label_count(a) == label_count(b)
        && a.split('.')
            .zip(b.split('.'))
            .all(|(x, y)| x == "*" || y == "*" || x == y)
}

/// The (scope, target) pairs a credential writes: each host or CIDR its rules
/// match crossed with each header or query param it sets. Mirrors what the
/// proxy actually writes, so two credentials conflict only when one would
/// overwrite the other at runtime.
fn claims(credential: &Credential) -> Vec<(Scope, String)> {
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
            Some(Scope::Host(
                host.trim().trim_end_matches('.').to_lowercase(),
            ))
        } else {
            rule.cidr
                .as_deref()
                .filter(|cidr| !cidr.trim().is_empty())
                .map(|value| Scope::Cidr(value.to_owned()))
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
    use rand::{Rng, SeedableRng, rngs::StdRng};
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

    fn gcp_id_token(id: i64, priority: i32, header: Option<&str>, hosts: &[&str]) -> Credential {
        credential(
            CredentialKind::GcpIdToken,
            id,
            priority,
            CredentialData::GcpIdToken(GcpIdTokenData {
                audience: "https://run.example".to_owned(),
                header: header.map(str::to_owned),
            }),
            hosts,
        )
    }

    fn aws(id: i64, priority: i32, hosts: &[&str]) -> Credential {
        credential(
            CredentialKind::AwsAuth,
            id,
            priority,
            CredentialData::AwsAuth(AwsAuthData {
                allowed_regions: vec![],
                allowed_services: vec![],
            }),
            hosts,
        )
    }

    fn hmac(id: i64, priority: i32, header: &str, hosts: &[&str]) -> Credential {
        credential(
            CredentialKind::Hmac,
            id,
            priority,
            CredentialData::Hmac(HmacData {
                timestamp_format: "unix".to_owned(),
                signature_algorithm: "sha256".to_owned(),
                signature_key_encoding: "raw".to_owned(),
                signature_output_encoding: "hex".to_owned(),
                signature_message: "{{ .Body }}".to_owned(),
                headers: vec![HmacHeader {
                    name: header.to_owned(),
                    value: "{{ .Signature }}".to_owned(),
                }],
                allow_chunked_body: false,
            }),
            hosts,
        )
    }

    fn replace(id: i64, priority: i32, match_headers: Value, hosts: &[&str]) -> Credential {
        let config = json!({ "proxy_value": "PLACEHOLDER", "match_headers": match_headers });
        static_with(id, priority, None, Some(config), hosts)
    }

    fn query(id: i64, priority: i32, param: &str, hosts: &[&str]) -> Credential {
        static_with(
            id,
            priority,
            Some(json!({ "query_param": param })),
            None,
            hosts,
        )
    }

    fn cidr(id: i64, priority: i32, cidr: &str) -> Credential {
        let mut credential = inject(id, priority, "Authorization", &[]);
        credential.rules = vec![RequestRule {
            host: None,
            cidr: Some(cidr.to_owned()),
            http_methods: vec![],
            paths: vec![],
        }];
        credential
    }

    #[test]
    fn resolves_conflicts() {
        const API: &[&str] = &["api.example.com"];
        let cases: Vec<(&str, Vec<Credential>, Vec<i64>)> = vec![
            (
                "a direct static secret beats a role transform",
                vec![gcp(1, ROLE, API), inject(2, DIRECT, "Authorization", API)],
                vec![2],
            ),
            (
                "a promoted role transform beats a direct static secret",
                vec![inject(1, DIRECT, "Authorization", API), gcp(2, 900, API)],
                vec![2],
            ),
            (
                "different headers on the same host both serve",
                vec![inject(1, DIRECT, "X-Api-Key", API), gcp(2, ROLE, API)],
                vec![1, 2],
            ),
            (
                "the same header on different hosts both serve",
                vec![
                    inject(1, DIRECT, "Authorization", API),
                    gcp(2, ROLE, &["other.example.com"]),
                ],
                vec![1, 2],
            ),
            (
                "equal priorities are left to the proxy",
                vec![inject(1, DIRECT, "Authorization", API), gcp(2, DIRECT, API)],
                vec![1, 2],
            ),
            (
                "header and host matching ignore case and trailing dots",
                vec![
                    inject(1, DIRECT, "AUTHORIZATION", &["API.Example.com."]),
                    gcp(2, ROLE, API),
                ],
                vec![1],
            ),
            (
                "a wildcard conflicts with one-label subdomains only",
                vec![
                    inject(1, DIRECT, "Authorization", &["*.googleapis.com"]),
                    gcp(2, ROLE, &["bigquery.googleapis.com"]),
                    oauth(3, ROLE, None, &["gmail.googleapis.com"]),
                    gcp(4, ROLE, &["googleapis.com"]),
                    gcp(5, ROLE, &["a.b.googleapis.com"]),
                ],
                vec![1, 4, 5],
            ),
            (
                "a bare * conflicts with every host",
                vec![
                    inject(1, DIRECT, "Authorization", &["*"]),
                    gcp(2, ROLE, &["any.example.com"]),
                ],
                vec![1],
            ),
            (
                "an OAuth token on a custom header does not claim Authorization",
                vec![
                    inject(1, DIRECT, "Authorization", API),
                    oauth(2, ROLE, Some("X-Goog-Api-Token"), API),
                ],
                vec![1, 2],
            ),
            (
                "an OAuth token claims its custom header",
                vec![
                    inject(1, DIRECT, "x-goog-api-token", API),
                    oauth(2, ROLE, Some("X-Goog-Api-Token"), API),
                ],
                vec![1],
            ),
            (
                "an OAuth token with a blank header claims Authorization",
                vec![
                    inject(1, DIRECT, "Authorization", API),
                    oauth(2, ROLE, Some(""), API),
                ],
                vec![1],
            ),
            (
                "GCP ID tokens claim their configured header or Authorization",
                vec![
                    inject(1, DIRECT, "Authorization", API),
                    gcp_id_token(2, ROLE, Some("x-serverless-authorization"), API),
                    gcp_id_token(3, ROLE, None, API),
                ],
                vec![1, 2],
            ),
            (
                "AWS claims Authorization",
                vec![inject(1, DIRECT, "Authorization", API), aws(2, ROLE, API)],
                vec![1],
            ),
            (
                "HMAC claims its signature headers",
                vec![
                    inject(1, DIRECT, "x-signature", API),
                    hmac(2, ROLE, "X-Signature", API),
                    hmac(3, ROLE, "X-Other-Signature", API),
                ],
                vec![1, 3],
            ),
            (
                "replace secrets claim only their match headers",
                vec![
                    replace(1, ROLE, json!(["Authorization"]), API),
                    replace(2, ROLE, json!([]), API),
                    inject(3, DIRECT, "Authorization", API),
                ],
                vec![2, 3],
            ),
            (
                "query params match exactly, including case",
                vec![
                    query(1, DIRECT, "key", API),
                    query(2, ROLE, "key", API),
                    query(3, ROLE, "KEY", API),
                ],
                vec![1, 3],
            ),
            (
                "CIDRs conflict only when identical",
                vec![
                    cidr(1, DIRECT, "10.0.0.0/8"),
                    cidr(2, ROLE, "10.0.0.0/8"),
                    cidr(3, ROLE, "10.1.0.0/16"),
                ],
                vec![1, 3],
            ),
            (
                "a withheld credential does not claim its other hosts",
                vec![
                    inject(1, 200, "Authorization", &["a.example.com"]),
                    gcp(2, DIRECT, &["a.example.com", "b.example.com"]),
                    inject(3, ROLE, "Authorization", &["b.example.com"]),
                ],
                vec![1, 3],
            ),
            (
                "credentials without scopes or targets never conflict",
                vec![
                    inject(1, DIRECT, "Authorization", &[]),
                    static_with(2, DIRECT, Some(json!({ "header": "" })), None, API),
                    gcp(3, ROLE, API),
                    credential(
                        CredentialKind::PgDsn,
                        4,
                        DIRECT,
                        CredentialData::PgDsn(PgDsnData {
                            foreign_id: "analytics".to_owned(),
                            database: "analytics".to_owned(),
                            role: None,
                            settings: vec![],
                        }),
                        API,
                    ),
                ],
                vec![1, 2, 3, 4],
            ),
        ];
        for (name, mut credentials, expected) in cases {
            suppress(&mut credentials);
            let served: Vec<i64> = credentials.iter().map(|credential| credential.id).collect();
            assert_eq!(served, expected, "{name}");
        }
    }

    /// The straightforward quadratic resolver `suppress` replaced: compare each
    /// claim with every stronger claim on the same target.
    fn suppress_by_scan(credentials: &[Credential]) -> Vec<i64> {
        let mut indexes: Vec<usize> = (0..credentials.len()).collect();
        indexes.sort_by_key(|&index| (-credentials[index].priority, -credentials[index].id));
        let mut claimed: Vec<(super::Scope, String, i32)> = Vec::new();
        let mut kept = Vec::new();
        for index in indexes {
            let credential = &credentials[index];
            let claims = super::claims(credential);
            let stronger = claims.iter().any(|(scope, target)| {
                claimed.iter().any(|(other, other_target, priority)| {
                    other_target == target
                        && *priority > credential.priority
                        && match (scope, other) {
                            (super::Scope::Host(a), super::Scope::Host(b)) => {
                                super::hosts_overlap(a, b)
                            }
                            (super::Scope::Cidr(a), super::Scope::Cidr(b)) => a == b,
                            _ => false,
                        }
                })
            });
            if !stronger {
                claimed.extend(
                    claims
                        .into_iter()
                        .map(|(scope, target)| (scope, target, credential.priority)),
                );
                kept.push(credential.id);
            }
        }
        kept.sort();
        kept
    }

    #[test]
    fn indexed_resolution_matches_a_full_scan() {
        const HOSTS: &[&str] = &[
            "api.example.com",
            "API.example.com.",
            "other.example.com",
            "example.com",
            "*.example.com",
            "*",
            "a.b.example.com",
            "*.b.example.com",
            "a.*.example.com",
            "*.*.com",
        ];
        const HEADERS: &[&str] = &["Authorization", "X-Api-Token", "x-api-token", "X-Signature"];
        // A fixed seed keeps failures reproducible.
        let mut rng = StdRng::seed_from_u64(0x5eed);
        let mut next = |bound: usize| rng.random_range(0..bound);
        for round in 0..500 {
            let credentials: Vec<Credential> = (0..1 + next(30))
                .map(|id| {
                    let header = HEADERS[next(HEADERS.len())];
                    let mut credential = match next(5) {
                        0 => inject(id as i64, 0, header, &[]),
                        1 => static_with(
                            id as i64,
                            0,
                            None,
                            Some(json!({ "proxy_value": "P", "match_headers": [header] })),
                            &[],
                        ),
                        2 => gcp(id as i64, 0, &[]),
                        3 => oauth(id as i64, 0, Some(header), &[]),
                        _ => static_with(
                            id as i64,
                            0,
                            Some(json!({ "query_param": "key" })),
                            None,
                            &[],
                        ),
                    };
                    credential.priority = [0, 0, 50, 100, 900][next(5)];
                    credential.rules = (0..next(3))
                        .map(|_| {
                            let cidr = next(6) == 0;
                            RequestRule {
                                host: (!cidr).then(|| HOSTS[next(HOSTS.len())].to_owned()),
                                cidr: cidr
                                    .then(|| ["10.0.0.0/8", "10.1.0.0/16"][next(2)].to_owned()),
                                http_methods: vec![],
                                paths: vec![],
                            }
                        })
                        .collect();
                    credential
                })
                .collect();
            let expected = suppress_by_scan(&credentials);
            let mut actual = credentials;
            suppress(&mut actual);
            let mut actual: Vec<i64> = actual.iter().map(|credential| credential.id).collect();
            actual.sort();
            assert_eq!(actual, expected, "round {round}");
        }
    }
}
