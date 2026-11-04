//! Round-trips harness-server's Codex App Server payloads through the upstream
//! types: whatever harness-server writes must parse as Codex's type and
//! serialize back unchanged, and whatever Codex clients send must parse into
//! harness-server's type without losing the fields it reads.

use std::collections::BTreeSet;
use std::path::PathBuf;

use codex_app_server_protocol as codex;
use harness_server::protocol::{
    self, ApprovalsReviewer, AskForApproval, InitializeResponse, SandboxPolicy,
    ThreadResumeResponse, ThreadStartResponse, TurnInterruptResponse, TurnStartResponse,
    TurnStatus, TurnSteerResponse,
};
use harness_server::{
    BridgeConfig, CodexTurnNormalizer, NormalizedContent, NormalizedEvent, NormalizedToolResult,
    notification_to_wire_value,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Asserts `ours` parses as the upstream type `T` and serializes back unchanged.
fn assert_round_trips<T: DeserializeOwned + Serialize>(ours: &impl Serialize) {
    let ours = serde_json::to_value(ours).expect("serialize harness-server value");
    let upstream: T = serde_json::from_value(ours.clone()).unwrap_or_else(|error| {
        panic!(
            "not a valid {}: {error}\n{ours:#}",
            std::any::type_name::<T>()
        )
    });
    let upstream = serde_json::to_value(upstream).expect("serialize upstream value");
    assert_eq!(
        upstream,
        ours,
        "{} changed shape",
        std::any::type_name::<T>()
    );
}

fn new_normalizer(turn_id: &str) -> CodexTurnNormalizer {
    let mut config = BridgeConfig::new("thread-1", turn_id);
    config.cwd = PathBuf::from("/workspace");
    CodexTurnNormalizer::new(config)
}

fn every_user_input() -> Vec<protocol::UserInput> {
    serde_json::from_value(json!([
        {"type": "text", "text": "hi", "text_elements": [
            {"byteRange": {"start": 0, "end": 2}, "placeholder": "@hi"}
        ]},
        {"type": "image", "url": "https://example.com/a.png", "detail": "low"},
        {"type": "localImage", "path": "/tmp/a.png"},
        {"type": "skill", "name": "review", "path": "/skills/review"},
        {"type": "mention", "name": "file", "path": "src/lib.rs"},
    ]))
    .expect("parse user input")
}

fn tool_use(raw_id: &str, tool: &str, arguments: Value) -> NormalizedEvent {
    NormalizedEvent::AssistantMessage {
        partial: false,
        stop_reason: Some("tool_use".to_string()),
        content: vec![NormalizedContent::ToolUse {
            raw_id: raw_id.to_string(),
            tool: tool.to_string(),
            arguments,
        }],
    }
}

fn tool_result(tool_use_id: &str, content: &str, is_error: bool) -> NormalizedEvent {
    NormalizedEvent::ToolResults(vec![NormalizedToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: content.to_string(),
        is_error,
        exit_code: None,
    }])
}

/// One turn that emits every notification and item type harness-server has.
fn notifications_for_every_kind() -> Vec<protocol::ServerNotification> {
    let mut normalizer = new_normalizer("turn-1");
    let mut out = normalizer.start_notifications(true).unwrap();
    out.extend(
        normalizer
            .emit_user_message(Some("client-1".to_string()), every_user_input())
            .unwrap(),
    );
    let events = [
        NormalizedEvent::AgentMessageStarted {
            item_id: "msg-1".to_string(),
            stop_reason: Some("tool_use".to_string()),
        },
        NormalizedEvent::AgentTextDelta {
            item_id: "msg-1".to_string(),
            delta: "Let me ".to_string(),
        },
        NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: Some("tool_use".to_string()),
            content: vec![
                NormalizedContent::ReasoningText {
                    item_id: "think-1".to_string(),
                    text: "plan".to_string(),
                },
                NormalizedContent::AgentText {
                    item_id: "msg-1".to_string(),
                    text: "Let me check.".to_string(),
                },
            ],
        },
        NormalizedEvent::ReasoningTextDelta {
            item_id: "think-2".to_string(),
            delta: "more".to_string(),
        },
        tool_use("bash-1", "Bash", json!({"command": "true"})),
        tool_result("bash-1", "ok", false),
        tool_use("bash-2", "Bash", json!({"command": "false"})),
        tool_result("bash-2", "Exit code 1\nfailed", true),
        tool_use("read-1", "Read", json!({"file_path": "README.md"})),
        tool_result("read-1", "contents", false),
        tool_use("read-2", "Read", json!({"file_path": "missing"})),
        tool_result("read-2", "not found", true),
        NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: Some("end_turn".to_string()),
            content: vec![NormalizedContent::AgentText {
                item_id: "msg-2".to_string(),
                text: "Done.".to_string(),
            }],
        },
        NormalizedEvent::Error {
            message: "rate limited".to_string(),
        },
    ];
    for event in &events {
        out.extend(normalizer.process_event(event).unwrap());
    }
    out.extend(normalizer.finish_turn(None).unwrap());

    let mut completed = new_normalizer("turn-2");
    out.extend(completed.start_notifications(false).unwrap());
    out.extend(completed.finish_turn(None).unwrap());

    let mut interrupted = new_normalizer("turn-3");
    out.extend(interrupted.start_notifications(false).unwrap());
    out.extend(interrupted.finish_turn_interrupted().unwrap());
    out
}

#[test]
fn server_notifications_match_codex() {
    let mut methods = BTreeSet::new();
    let mut item_types = BTreeSet::new();
    for notification in notifications_for_every_kind() {
        let wire = notification_to_wire_value(&notification).unwrap();
        assert_round_trips::<codex::ServerNotification>(&wire);
        methods.insert(wire["method"].as_str().unwrap().to_string());
        if let Some(item_type) = wire["params"]["item"]["type"].as_str() {
            item_types.insert(item_type.to_string());
        }
    }

    // Keeps the fixture honest when harness-server learns a new notification.
    assert_eq!(
        methods,
        BTreeSet::from(
            [
                "error",
                "item/agentMessage/delta",
                "item/completed",
                "item/reasoning/textDelta",
                "item/started",
                "thread/started",
                "turn/completed",
                "turn/started",
            ]
            .map(String::from)
        )
    );
    assert_eq!(
        item_types,
        BTreeSet::from(
            [
                "agentMessage",
                "commandExecution",
                "dynamicToolCall",
                "reasoning",
                "userMessage",
            ]
            .map(String::from)
        )
    );
}

#[test]
fn responses_match_codex() {
    let mut normalizer = new_normalizer("turn-1");
    normalizer
        .emit_user_message(None, every_user_input())
        .unwrap();
    let mut thread = normalizer.thread_snapshot().unwrap();
    thread.turns = vec![normalizer.turn_snapshot(TurnStatus::Completed)];
    let thread_response = ThreadStartResponse {
        thread,
        model: "gpt-5".to_string(),
        model_provider: "openai".to_string(),
        service_tier: Some("flex".to_string()),
        cwd: PathBuf::from("/workspace"),
        runtime_workspace_roots: Vec::new(),
        instruction_sources: Vec::new(),
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        sandbox: SandboxPolicy::DangerFullAccess,
        active_permission_profile: None,
        reasoning_effort: None,
    };

    assert_round_trips::<codex::InitializeResponse>(&InitializeResponse {
        user_agent: "harness-server".to_string(),
        codex_home: PathBuf::from("/home/agent/.codex"),
        platform_family: "unix".to_string(),
        platform_os: "linux".to_string(),
    });
    assert_round_trips::<codex::ThreadStartResponse>(&thread_response);
    assert_round_trips::<codex::ThreadResumeResponse>(&ThreadResumeResponse {
        response: thread_response,
        initial_turns_page: None,
    });
    assert_round_trips::<codex::TurnStartResponse>(&TurnStartResponse {
        turn: normalizer.turn_snapshot(TurnStatus::InProgress),
    });
    assert_round_trips::<codex::TurnSteerResponse>(&TurnSteerResponse {
        turn_id: "turn-1".to_string(),
    });
    assert_round_trips::<codex::TurnInterruptResponse>(&TurnInterruptResponse {});
}

/// Parses one request's params as both the upstream and harness-server types.
fn parse_both<T: DeserializeOwned, U: DeserializeOwned>(params: Value) -> (T, U) {
    let upstream = serde_json::from_value(params.clone()).expect("upstream params");
    let ours = serde_json::from_value(params).expect("harness-server params");
    (upstream, ours)
}

fn to_json(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn request_params_match_codex() {
    let input = to_json(every_user_input());

    let (upstream, ours): (codex::ThreadStartParams, protocol::ThreadStartParams) =
        parse_both(json!({
            "model": "gpt-5",
            "modelProvider": "openai",
            "serviceTier": "flex",
            "cwd": "/workspace",
            "approvalPolicy": "never",
        }));
    assert_eq!(ours.model, upstream.model);
    assert_eq!(ours.model_provider, upstream.model_provider);
    assert_eq!(ours.service_tier, upstream.service_tier);
    assert_eq!(ours.cwd, upstream.cwd);

    for service_tier in [json!(null), json!("flex")] {
        let (upstream, ours): (codex::ThreadResumeParams, protocol::ThreadResumeParams) =
            parse_both(json!({
                "threadId": "thread-1",
                "model": "gpt-5",
                "modelProvider": "openai",
                "serviceTier": service_tier,
                "cwd": "/workspace",
                "excludeTurns": true,
            }));
        assert_eq!(ours.thread_id, upstream.thread_id);
        assert_eq!(ours.model, upstream.model);
        assert_eq!(ours.model_provider, upstream.model_provider);
        assert_eq!(ours.service_tier, upstream.service_tier);
        assert_eq!(ours.cwd, upstream.cwd);
        assert_eq!(ours.exclude_turns, upstream.exclude_turns);
    }
    let (upstream, ours): (codex::ThreadResumeParams, protocol::ThreadResumeParams) =
        parse_both(json!({"threadId": "thread-1"}));
    assert_eq!(ours.service_tier, upstream.service_tier);
    assert_eq!(ours.exclude_turns, upstream.exclude_turns);

    for effort in ["none", "minimal", "low", "medium", "high", "xhigh"] {
        let (upstream, ours): (codex::TurnStartParams, protocol::TurnStartParams) =
            parse_both(json!({
                "threadId": "thread-1",
                "clientUserMessageId": "client-1",
                "input": input,
                "effort": effort,
            }));
        assert_eq!(ours.thread_id, upstream.thread_id);
        assert_eq!(ours.client_user_message_id, upstream.client_user_message_id);
        assert_eq!(to_json(&ours.input), to_json(&upstream.input));
        assert_eq!(to_json(ours.effort), to_json(upstream.effort));
    }

    let (upstream, ours): (codex::TurnSteerParams, protocol::TurnSteerParams) = parse_both(json!({
        "threadId": "thread-1",
        "clientUserMessageId": "client-2",
        "input": input,
        "expectedTurnId": "turn-1",
    }));
    assert_eq!(ours.thread_id, upstream.thread_id);
    assert_eq!(ours.client_user_message_id, upstream.client_user_message_id);
    assert_eq!(to_json(&ours.input), to_json(&upstream.input));
    assert_eq!(ours.expected_turn_id, upstream.expected_turn_id);

    let (upstream, ours): (codex::TurnInterruptParams, protocol::TurnInterruptParams) =
        parse_both(json!({"threadId": "thread-1", "turnId": "turn-1"}));
    assert_eq!(ours.thread_id, upstream.thread_id);
    assert_eq!(ours.turn_id, upstream.turn_id);
}
