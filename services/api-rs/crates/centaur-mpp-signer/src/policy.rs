use std::collections::HashSet;

use crate::model::{Endpoint, PolicyEffect, PolicyRule, Service};

#[derive(Clone, Debug)]
pub struct Policy {
    default_methods: HashSet<String>,
    rules: Vec<PolicyRule>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Decision {
    pub allowed: bool,
    pub reason: &'static str,
}

impl Policy {
    pub fn new(default_methods: Vec<String>, rules: Vec<PolicyRule>) -> anyhow::Result<Self> {
        let default_methods = default_methods
            .into_iter()
            .map(|method| method.to_ascii_uppercase())
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            !default_methods.is_empty(),
            "MPP default methods cannot be empty"
        );
        Ok(Self {
            default_methods,
            rules,
        })
    }

    pub fn decide(&self, service: &Service, endpoint: &Endpoint) -> Decision {
        let mut allowed_by_rule = false;
        for rule in &self.rules {
            if !rule_matches(rule, service, endpoint) {
                continue;
            }
            match rule.effect {
                PolicyEffect::Deny => {
                    return Decision {
                        allowed: false,
                        reason: "policy_denied",
                    };
                }
                PolicyEffect::Allow => allowed_by_rule = true,
            }
        }
        if allowed_by_rule {
            return Decision {
                allowed: true,
                reason: "policy_allowed",
            };
        }
        if self
            .default_methods
            .contains(&endpoint.method.to_ascii_uppercase())
        {
            Decision {
                allowed: true,
                reason: "default_method_allowed",
            }
        } else {
            Decision {
                allowed: false,
                reason: "method_not_allowed",
            }
        }
    }
}

fn rule_matches(rule: &PolicyRule, service: &Service, endpoint: &Endpoint) -> bool {
    if let Some(pattern) = &rule.service
        && !wildcard_matches(pattern, &service.id)
    {
        return false;
    }
    if let Some(category) = &rule.category
        && !service
            .categories
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(category))
    {
        return false;
    }
    if let Some(pattern) = &rule.realm {
        let realm = service
            .realm
            .clone()
            .or_else(|| {
                service
                    .base_url()
                    .and_then(|value| reqwest::Url::parse(value).ok())
                    .and_then(|url| url.host_str().map(str::to_owned))
            })
            .unwrap_or_default();
        if !wildcard_matches(pattern, &realm) {
            return false;
        }
    }
    if let Some(methods) = &rule.methods
        && !methods
            .iter()
            .any(|method| method.eq_ignore_ascii_case(&endpoint.method))
    {
        return false;
    }
    if let Some(pattern) = &rule.path
        && !wildcard_matches(pattern, &endpoint.path)
    {
        return false;
    }
    true
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for &token in pattern {
        let mut current = vec![false; value.len() + 1];
        if token == b'*' {
            current[0] = previous[0];
            for index in 1..=value.len() {
                current[index] = previous[index] || current[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                current[index] = previous[index - 1] && token == value[index - 1];
            }
        }
        previous = current;
    }
    previous[value.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Endpoint, PolicyEffect, PolicyRule, Service};

    fn service() -> Service {
        Service {
            id: "catalog".to_owned(),
            name: None,
            description: None,
            service_url: Some("https://api.example".to_owned()),
            url: None,
            realm: Some("api.example".to_owned()),
            categories: vec!["data".to_owned()],
            tags: vec![],
            status: Some("active".to_owned()),
            endpoints: vec![],
            extra: Default::default(),
        }
    }

    fn endpoint(method: &str) -> Endpoint {
        Endpoint {
            method: method.to_owned(),
            path: "/v1/records".to_owned(),
            description: None,
            payment: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn default_get_and_explicit_post_allow() {
        let policy = Policy::new(
            vec!["GET".to_owned()],
            vec![PolicyRule {
                effect: PolicyEffect::Allow,
                service: Some("catalog".to_owned()),
                category: None,
                realm: None,
                methods: Some(vec!["POST".to_owned()]),
                path: Some("/v1/*".to_owned()),
            }],
        )
        .unwrap();

        assert!(policy.decide(&service(), &endpoint("GET")).allowed);
        assert!(policy.decide(&service(), &endpoint("POST")).allowed);
        assert!(!policy.decide(&service(), &endpoint("DELETE")).allowed);
    }

    #[test]
    fn deny_wins_over_allow() {
        let rules = vec![
            PolicyRule {
                effect: PolicyEffect::Allow,
                service: Some("*".to_owned()),
                category: None,
                realm: None,
                methods: Some(vec!["POST".to_owned()]),
                path: Some("/v1/*".to_owned()),
            },
            PolicyRule {
                effect: PolicyEffect::Deny,
                service: Some("catalog".to_owned()),
                category: Some("data".to_owned()),
                realm: Some("api.*".to_owned()),
                methods: Some(vec!["POST".to_owned()]),
                path: Some("/v1/records".to_owned()),
            },
        ];
        let policy = Policy::new(vec!["GET".to_owned()], rules).unwrap();

        assert_eq!(
            policy.decide(&service(), &endpoint("POST")),
            Decision {
                allowed: false,
                reason: "policy_denied"
            }
        );
    }

    #[test]
    fn realm_rules_fall_back_to_registered_service_host() {
        let mut service = service();
        service.realm = None;
        let policy = Policy::new(
            vec!["GET".to_owned()],
            vec![PolicyRule {
                effect: PolicyEffect::Deny,
                service: None,
                category: None,
                realm: Some("api.*".to_owned()),
                methods: Some(vec!["GET".to_owned()]),
                path: None,
            }],
        )
        .unwrap();

        assert_eq!(
            policy.decide(&service, &endpoint("GET")),
            Decision {
                allowed: false,
                reason: "policy_denied"
            }
        );
    }
}
