//! The executor against a fake tenancy proxy: turn tokens gate everything,
//! and every connector request goes through the proxy with the turn token.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    extract::Request,
    http::{Method, StatusCode},
    response::IntoResponse,
};
use centaur_codemode_protocol::token::{
    ToolPolicy, TurnGrant, TurnTokenSigner, TurnTokenVerifier, testing,
};
use serde_json::{Value, json};

use crate::{catalog::Catalog, engine::Engine, executor};

const CONNECTOR: &str = r#"
import type { Ctx } from "../centaur";

/** GET a URL and return what came back. */
export async function get(args: { url: string }, ctx: Ctx): Promise<{ status: number; body: string }> {
  const res = await ctx.fetch(args.url);
  return { status: res.status, body: await res.text() };
}

/** The calling context's thread and principal. */
export async function context(args: {}, ctx: Ctx): Promise<{ thread_key: string | null; principal: string | null; harness: string | null }> {
  return {
    thread_key: ctx.context?.thread_key ?? null,
    principal: ctx.context?.sandbox?.principal_id ?? null,
    harness: ctx.context?.harness ?? null,
  };
}
"#;

const OTHER: &str = r#"
import type { Ctx } from "../centaur";

/** Another connector a policy may hide. */
export async function ping(args: {}, ctx: Ctx): Promise<{ pong: boolean }> {
  return { pong: true };
}
"#;

/// What the fake proxy saw: method, target, and Proxy-Authorization.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// A fake tenancy proxy. Absolute-form requests are answered directly (the
/// proxy plays every upstream); a CONNECT is recorded and refused, which is
/// enough to see what credential the tunnel carried.
async fn fake_proxy() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let record = seen.clone();
    let app = Router::new().fallback(move |request: Request| {
        let record = record.clone();
        async move {
            let auth = request
                .headers()
                .get("proxy-authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            record.lock().unwrap().push((
                request.method().to_string(),
                request.uri().to_string(),
                auth,
            ));
            if request.method() == Method::CONNECT {
                return StatusCode::FORBIDDEN.into_response();
            }
            if request.uri().path() == "/api/v1/sandbox/permissions" {
                return axum::Json(json!({ "data": { "principal_id": "prn_from_console" } }))
                    .into_response();
            }
            format!("upstream {}", request.uri().path()).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), seen)
}

struct Fixture {
    _dir: tempfile::TempDir,
    url: String,
    signer: TurnTokenSigner,
    seen: Seen,
}

async fn executor() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in [("demo", CONNECTOR), ("other", OTHER)] {
        let connector = dir.path().join("connectors").join(name);
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(
            connector.join("connector.toml"),
            format!("description = \"{name} connector\"\n"),
        )
        .unwrap();
        std::fs::write(connector.join("index.ts"), source).unwrap();
    }
    let catalog = Catalog::load(&[dir.path().join("connectors")], false).unwrap();
    let engine = Engine::new(Arc::new(catalog), None, None, None).unwrap();
    let (proxy, seen) = fake_proxy().await;
    let (private, public) = testing::pair();
    let app = executor::router(
        engine,
        executor::Config {
            verifier: TurnTokenVerifier::from_pem(public.as_bytes()).unwrap(),
            proxy_url: proxy,
            console_url: Some("http://console.test".to_owned()),
            max_cells: 4,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture {
        _dir: dir,
        url: format!("http://{address}"),
        signer: TurnTokenSigner::from_pem(private.as_bytes()).unwrap(),
        seen,
    }
}

impl Fixture {
    fn token(&self, principal: &str, tools: ToolPolicy) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.signer
            .sign(
                &TurnGrant {
                    principal: principal.to_owned(),
                    requester: None,
                    thread_key: format!("slack:T1:C1:{principal}"),
                    execution_id: "exe_1".to_owned(),
                    tools,
                },
                now,
            )
            .unwrap()
            .0
    }

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        let mut request = reqwest::Client::new()
            .post(format!("{}{path}", self.url))
            .json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn exec(&self, token: &str, code: &str) -> Value {
        let (status, body) = self
            .post(
                "/v1/exec",
                Some(token),
                json!({ "code": code, "cell_id": "call_1" }),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    fn seen(&self) -> Vec<(String, String, String)> {
        self.seen.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn nothing_runs_without_a_valid_turn_token() {
    let fx = executor().await;
    let (other, _) = testing::pair();
    let forged = TurnTokenSigner::from_pem(other.as_bytes())
        .unwrap()
        .sign(
            &TurnGrant {
                principal: "prn_x".to_owned(),
                requester: None,
                thread_key: "t".to_owned(),
                execution_id: "e".to_owned(),
                tools: ToolPolicy::default(),
            },
            0,
        )
        .unwrap()
        .0;
    for path in ["/v1/exec", "/v1/search", "/v1/load"] {
        for token in [None, Some("not-a-token"), Some(forged.as_str())] {
            let (status, _) = fx
                .post(
                    path,
                    token,
                    json!({ "code": "return 1;", "cell_id": "c", "query": "q", "tool": "demo" }),
                )
                .await;
            assert_eq!(status, 401, "{path} {token:?}");
        }
    }
    assert!(fx.seen().is_empty(), "nothing reached the proxy");
}

#[tokio::test]
async fn connector_requests_go_through_the_proxy_with_the_turn_token() {
    let fx = executor().await;
    let token_a = fx.token("prn_a", ToolPolicy::default());
    let token_b = fx.token("prn_b", ToolPolicy::default());

    let body = fx
        .exec(
            &token_a,
            r#"return await tools.demo.get({ url: "http://api.example.test/v1/x" });"#,
        )
        .await;
    assert_eq!(
        body["result"],
        json!({ "status": 200, "body": "upstream /v1/x" }),
        "{body}"
    );
    fx.exec(
        &token_b,
        r#"return await tools.demo.get({ url: "http://api.example.test/v1/y" });"#,
    )
    .await;
    // HTTPS goes through a CONNECT tunnel carrying the same credential.
    let body = fx
        .exec(&token_a, r#"try { await tools.demo.get({ url: "https://secure.example.test/" }); } catch (e) { return String(e); }"#)
        .await;
    assert!(body["result"].as_str().is_some(), "{body}");

    let seen = fx.seen();
    let request = |target: &str| {
        seen.iter()
            .find(|(_, uri, _)| uri.contains(target))
            .unwrap_or_else(|| panic!("{target} never reached the proxy: {seen:?}"))
            .clone()
    };
    assert_eq!(request("/v1/x").2, format!("Bearer {token_a}"));
    assert_eq!(request("/v1/y").2, format!("Bearer {token_b}"));
    let (method, target, auth) = request("secure.example.test");
    assert_eq!(
        (method.as_str(), target.as_str()),
        ("CONNECT", "secure.example.test:443")
    );
    assert_eq!(auth, format!("Bearer {token_a}"));
}

#[tokio::test]
async fn the_turn_token_decides_policy_and_context() {
    let fx = executor().await;
    let token = fx.token(
        "prn_a",
        ToolPolicy {
            allowlist: None,
            blocklist: Some(vec!["other".to_owned()]),
        },
    );
    let body = fx
        .exec(
            &token,
            r#"return { tools: Object.keys(tools), context: await tools.demo.context({}) };"#,
        )
        .await;
    assert_eq!(body["result"]["tools"], json!(["demo"]), "{body}");
    assert_eq!(
        body["result"]["context"],
        json!({ "thread_key": "slack:T1:C1:prn_a", "principal": "prn_from_console", "harness": "codemode" })
    );
    // Console permissions came through the proxy, with the turn's credential.
    assert!(
        fx.seen()
            .iter()
            .any(|(_, uri, auth)| uri.contains("/api/v1/sandbox/permissions")
                && *auth == format!("Bearer {token}"))
    );

    let (status, search) = fx
        .post(
            "/v1/search",
            Some(&token),
            json!({ "query": "ping another connector" }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(
        search["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["tool"] == "demo"),
        "{search}"
    );
    let (status, _) = fx
        .post("/v1/load", Some(&token), json!({ "tool": "other" }))
        .await;
    assert_eq!(status, 404);
    let (status, load) = fx
        .post("/v1/load", Some(&token), json!({ "tool": "demo" }))
        .await;
    assert_eq!(status, 200);
    assert!(load["brief"].as_str().unwrap().contains("get(args"));
}
