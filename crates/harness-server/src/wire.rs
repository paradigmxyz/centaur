use codex_app_server_protocol::{JSONRPCMessage, JSONRPCNotification, ServerNotification};
use serde_json::Value;

use crate::Result;

pub fn is_known_untyped_server_notification(method: &str) -> bool {
    matches!(method, "remoteControl/status/changed")
}

pub fn notification_to_jsonrpc(notification: &ServerNotification) -> Result<JSONRPCNotification> {
    let value = serde_json::to_value(notification)?;
    Ok(serde_json::from_value(value)?)
}

pub fn notification_to_wire_value(notification: &ServerNotification) -> Result<Value> {
    let rpc = notification_to_jsonrpc(notification)?;
    Ok(serde_json::to_value(JSONRPCMessage::Notification(rpc))?)
}
