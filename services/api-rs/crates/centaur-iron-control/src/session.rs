//! Per-session principal registration.
//!
//! Roles are registered once at startup (see [`crate::register_role`]); a
//! [`SessionRegistrar`] carries the resulting role OIDs and, when a session
//! starts, upserts the session's principal. Brand-new principals receive the
//! default roles once; existing principals keep their current assignments so
//! operator revocations in console or ``centaur-perms`` remain sticky. The
//! principal is derived from the thread key (see [`crate::derive_principal`]).

use crate::IronControlClient;
use crate::error::{IronControlError, Result};
use crate::models::Principal;
use crate::principal::derive_principal;

/// Registers a session's principal against iron-control at session start.
///
/// Cheap to clone (the inner [`IronControlClient`] shares a connection pool),
/// so it can live on a shared runtime handle.
#[derive(Clone, Debug)]
pub struct SessionRegistrar {
    client: IronControlClient,
    namespace: String,
    assign_role_ids: Vec<String>,
}

impl SessionRegistrar {
    /// ``assign_role_ids`` are the iron-control role OIDs (from
    /// [`crate::register_role`]) to assign to every session's principal.
    pub fn new(
        client: IronControlClient,
        namespace: impl Into<String>,
        assign_role_ids: Vec<String>,
    ) -> Self {
        Self {
            client,
            namespace: namespace.into(),
            assign_role_ids,
        }
    }

    /// Upsert the principal for ``thread_key``. ``slack_user_id`` keys a 1:1
    /// DM principal; it is ignored for channel threads. ``conversation_name``
    /// is the human-readable channel/DM name (when the slackbot resolved one)
    /// used as the principal's display name. Returns the upserted principal
    /// record (its ``id`` is the OID) so callers can bind the session's egress
    /// proxy to the same identity.
    ///
    /// Default roles are assigned only when the principal does not already
    /// exist. Re-registering an existing channel/user still refreshes identity
    /// metadata, but it must not restore roles that an operator manually
    /// removed.
    pub async fn register_session(
        &self,
        thread_key: &str,
        slack_user_id: Option<&str>,
        conversation_name: Option<&str>,
    ) -> Result<Principal> {
        let principal = derive_principal(thread_key, slack_user_id, conversation_name);
        let input = principal.to_identity_input(&self.namespace);
        let exists = match self
            .client
            .get_principal(&self.namespace, &input.foreign_id)
            .await
        {
            Ok(_) => true,
            Err(error) if is_status(&error, 404) => false,
            Err(error) => return Err(error),
        };
        let record = self.client.upsert_principal(&input).await?;
        if !exists {
            for role_id in &self.assign_role_ids {
                match self.client.assign_role(&record.id, role_id).await {
                    Ok(()) => {}
                    Err(error) if is_status(&error, 409) || is_status(&error, 422) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(record)
    }
}

fn is_status(err: &IronControlError, code: u16) -> bool {
    matches!(err, IronControlError::Status { status, .. } if *status == code)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn register_session_seeds_roles_for_new_principal() {
        let (base_url, requests, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(
            IronControlClient::new(base_url, "test-key"),
            "default",
            vec!["role_infra".to_owned()],
        );

        registrar
            .register_session(
                "slack:T123:C123:1773364194.179929",
                Some("U123"),
                Some("general"),
            )
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(
                &"GET /api/v1/principals/lookup/default/slack-channel-t123-c123".to_owned()
            )
        );
        assert!(requests.contains(&"PUT /api/v1/principals/slack-channel-t123-c123".to_owned()));
        assert!(requests.contains(&"POST /api/v1/principals/prn_channel/roles".to_owned()));
        server.abort();
    }

    #[tokio::test]
    async fn register_session_does_not_restore_roles_for_existing_principal() {
        let (base_url, requests, server) = spawn_iron_control_stub(true).await;
        let registrar = SessionRegistrar::new(
            IronControlClient::new(base_url, "test-key"),
            "default",
            vec!["role_infra".to_owned()],
        );

        registrar
            .register_session(
                "slack:T123:C123:1773364194.179929",
                Some("U123"),
                Some("general"),
            )
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(
                &"GET /api/v1/principals/lookup/default/slack-channel-t123-c123".to_owned()
            )
        );
        assert!(requests.contains(&"PUT /api/v1/principals/slack-channel-t123-c123".to_owned()));
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_channel/roles"),
            "existing principals must not have manually removed roles restored"
        );
        server.abort();
    }

    async fn spawn_iron_control_stub(
        principal_exists: bool,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buf[..read]),
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let first_line = request.lines().next().unwrap_or_default();
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or_default();
                let path = parts.next().unwrap_or_default();
                seen.lock().unwrap().push(format!("{method} {path}"));

                let (status_line, body) = match (method, path) {
                    ("GET", "/api/v1/principals/lookup/default/slack-channel-t123-c123")
                        if principal_exists =>
                    {
                        ("200 OK", principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/default/slack-channel-t123-c123") => {
                        ("404 Not Found", r#"{"error":"not found"}"#.to_owned())
                    }
                    ("PUT", "/api/v1/principals/slack-channel-t123-c123") => {
                        ("200 OK", principal_body())
                    }
                    ("POST", "/api/v1/principals/prn_channel/roles") => {
                        ("200 OK", r#"{"data":{"ok":true}}"#.to_owned())
                    }
                    _ => (
                        "500 Internal Server Error",
                        r#"{"error":"unexpected"}"#.to_owned(),
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (base_url, requests, handle)
    }

    fn principal_body() -> String {
        r#"{"data":{"id":"prn_channel","namespace":"default","foreign_id":"slack-channel-t123-c123","name":"Slack Channel #general","labels":{}}}"#.to_owned()
    }
}
