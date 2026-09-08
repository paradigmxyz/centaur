//! Authenticate workflow-created buttons without exposing the signing key to Python.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;

use crate::{CreateWorkflowRunRequest, WorkflowRuntimeError};

const PREFIX: &str = "centaur.workflow.action:";
const INVALID: &str = "invalid or untrusted workflow button";

#[derive(Deserialize, Serialize)]
struct Target {
    workflow_name: String,
    input: Map<String, Value>,
}

#[derive(Deserialize, Serialize)]
struct Claims {
    group_id: String,
    action: String,
    channel_id: String,
    #[serde(flatten)]
    target: Target,
}

#[derive(Deserialize)]
pub struct Invocation {
    pub button: String,
    pub click: Value,
    pub idempotency_key: String,
}

fn button_mac(secret: &[u8]) -> Hmac<Sha256> {
    let mut derive = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    derive.update(b"centaur:workflow-buttons:v1");
    Hmac::<Sha256>::new_from_slice(&derive.finalize().into_bytes())
        .expect("HMAC accepts a 32-byte key")
}

/// Called only on messages posted through the owning workflow's context RPC.
/// Agent tools and the Slack HTTP proxy do not pass through this function.
pub fn sign_message(message: &mut Value, secret: &[u8]) -> Result<(), WorkflowRuntimeError> {
    let channel = message["channel"].as_str().unwrap_or_default().to_owned();
    fn visit(value: &mut Value, channel: &str, secret: &[u8]) -> Result<(), WorkflowRuntimeError> {
        if value["type"] == "button"
            && let Some(action_id) = value["action_id"]
                .as_str()
                .and_then(|s| s.strip_prefix(PREFIX))
        {
            let invalid = || WorkflowRuntimeError::BadRequest(INVALID.into());
            let (group, action) = action_id.split_once(':').ok_or_else(invalid)?;
            if uuid::Uuid::parse_str(group).is_err()
                || action.is_empty()
                || !channel.starts_with(['C', 'D', 'G'])
                || !channel.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err(invalid());
            }
            let target: Target = serde_json::from_str(value["value"].as_str().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            if target.workflow_name.trim().is_empty() || secret.is_empty() {
                return Err(invalid());
            }
            let claims = Claims {
                group_id: group.into(),
                action: action.into(),
                channel_id: channel.into(),
                target,
            };
            let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
            let authenticated = format!("v1.{payload}");
            let mut mac = button_mac(secret);
            mac.update(authenticated.as_bytes());
            let token = format!(
                "{authenticated}.{}",
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            );
            if token.len() > 2000 {
                return Err(WorkflowRuntimeError::BadRequest(
                    "signed Slack button value exceeds 2000 bytes; reduce workflow input".into(),
                ));
            }
            value["value"] = json!(token);
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, channel, secret)?;
                }
            }
            Value::Object(values) => {
                for value in values.values_mut() {
                    visit(value, channel, secret)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    if let Some(blocks) = message.get_mut("blocks") {
        visit(blocks, &channel, secret)?;
    }
    Ok(())
}

pub fn verify(
    invocation: Invocation,
    secret: &[u8],
) -> Result<CreateWorkflowRunRequest, &'static str> {
    if secret.is_empty() || invocation.button.len() > 2000 || !invocation.button.starts_with("v1.")
    {
        return Err(INVALID);
    }
    let (authenticated, signature) = invocation.button.rsplit_once('.').ok_or(INVALID)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| INVALID)?;
    let mut mac = button_mac(secret);
    mac.update(authenticated.as_bytes());
    mac.verify_slice(&signature).map_err(|_| INVALID)?;
    let payload = URL_SAFE_NO_PAD
        .decode(authenticated.strip_prefix("v1.").ok_or(INVALID)?)
        .map_err(|_| INVALID)?;
    let claims: Claims = serde_json::from_slice(&payload).map_err(|_| INVALID)?;
    if invocation.click["id"] != claims.group_id
        || invocation.click["action"] != claims.action
        || invocation.click["channel_id"] != claims.channel_id
        || invocation.idempotency_key.is_empty()
    {
        return Err(INVALID);
    }
    let mut input = claims.target.input;
    input.insert("click".into(), invocation.click);
    Ok(CreateWorkflowRunRequest {
        workflow_name: claims.target.workflow_name,
        input: Value::Object(input),
        idempotency_key: Some(invocation.idempotency_key),
        harness_type: None,
        max_attempts: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Value {
        json!({"channel": "C1", "blocks": [{"type": "actions", "elements": [{
            "type": "button", "action_id": "centaur.workflow.action:00000000-0000-0000-0000-000000000001:approve",
            "value": json!({"workflow_name": "review", "input": {"release_id": "r1", "click": {"user_id": "FORGED"}}}).to_string(),
        }]}]})
    }

    fn invocation(button: String) -> Invocation {
        Invocation {
            button,
            idempotency_key: "delivery-1".into(),
            click: json!({"id": "00000000-0000-0000-0000-000000000001", "action": "approve", "channel_id": "C1", "user_id": "U1"}),
        }
    }

    #[test]
    fn signatures_bind_target_input_action_group_and_destination() {
        let mut posted = message();
        sign_message(&mut posted, b"test-secret").unwrap();
        let token = posted["blocks"][0]["elements"][0]["value"]
            .as_str()
            .unwrap()
            .to_owned();
        let run = verify(invocation(token.clone()), b"test-secret").unwrap();
        assert_eq!(run.workflow_name, "review");
        assert_eq!(run.input["release_id"], "r1");
        assert_eq!(run.input["click"]["user_id"], "U1");
        assert_eq!(run.idempotency_key.as_deref(), Some("delivery-1"));
        assert!(verify(invocation(token.clone()), b"other-secret").is_err());
        for field in ["action", "id", "channel_id"] {
            let mut forged = invocation(token.clone());
            forged.click[field] = json!("OTHER");
            assert!(verify(forged, b"test-secret").is_err(), "{field}");
        }
        let parts: Vec<_> = token.split('.').collect();
        let original: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        for field in ["workflow_name", "input", "group_id", "action", "channel_id"] {
            let mut tampered = original.clone();
            tampered[field] = if field == "input" {
                json!({"release_id": "attacker-chosen"})
            } else {
                json!("OTHER")
            };
            let forged = format!(
                "v1.{}.{}",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered).unwrap()),
                parts[2]
            );
            assert!(
                verify(invocation(forged), b"test-secret").is_err(),
                "{field}"
            );
        }
        for forged in [
            String::new(),
            original.to_string(),
            token.replace("v1.", "v2."),
            format!("{}.bad", token),
            "x".repeat(2001),
        ] {
            assert!(verify(invocation(forged), b"test-secret").is_err());
        }
    }

    #[test]
    fn signing_respects_slack_size_limits_and_requires_a_channel_id() {
        let mut posted = message();
        posted["channel"] = json!("#general");
        assert!(sign_message(&mut posted, b"secret").is_err());
        let mut posted = message();
        posted["blocks"][0]["elements"][0]["value"] = json!(
            json!({"workflow_name": "review", "input": {"large": "x".repeat(1800)}}).to_string()
        );
        assert!(sign_message(&mut posted, b"secret").is_err());
        assert!(sign_message(&mut message(), b"").is_err());
        let mut plain = json!({"channel": "#general", "text": "hello"});
        sign_message(&mut plain, b"").unwrap();
        assert_eq!(plain, json!({"channel": "#general", "text": "hello"}));
    }
}
