use std::collections::HashMap;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_app_server_protocol::{
    ApprovalsReviewer, AskForApproval, ClientResponse, InitializeResponse, JSONRPCError,
    JSONRPCErrorError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse, RequestId, SandboxPolicy,
    ServerNotification, ThreadResumeParams, ThreadResumeResponse, ThreadStartParams,
    ThreadStartResponse, TurnInterruptParams, TurnInterruptResponse, TurnStartParams,
    TurnStartResponse, TurnStatus, TurnSteerParams, TurnSteerResponse, UserInput,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::amp::AmpHarness;
use crate::claude::ClaudeCodeHarness;
use crate::codex::CodexHarnessServer;
use crate::traits::{
    AppServerNormalizer, AppServerRuntime, HarnessChild, HarnessKind, HarnessServer,
    NormalizedEvent, ThreadState,
};
use crate::turn::{BridgeConfig, CodexTurnNormalizer};
use crate::util::{absolute_path, default_codex_home, write_value};
use crate::wire::{is_known_untyped_server_notification, notification_to_wire_value};
use crate::{HarnessServerError, Result};

pub fn server_for(kind: HarnessKind) -> Box<dyn AppServerRuntime> {
    match kind {
        HarnessKind::Codex => Box::new(CodexHarnessServer),
        HarnessKind::ClaudeCode => Box::new(AppServerNormalizer::new(ClaudeCodeHarness)),
        HarnessKind::Amp => Box::new(AppServerNormalizer::new(AmpHarness)),
    }
}

pub fn run_harness_server(kind: HarnessKind) -> Result<()> {
    server_for(kind).run_stdio()
}

pub fn run_blocks_server(kind: HarnessKind) -> Result<()> {
    match kind {
        HarnessKind::Codex => crate::codex::run_codex_blocks_server(),
        HarnessKind::ClaudeCode => run_blocks_app_server(&ClaudeCodeHarness),
        HarnessKind::Amp => run_blocks_app_server(&AmpHarness),
    }
}

pub fn run_validate_jsonrpc() -> Result<()> {
    let stdin = io::stdin();
    for raw in stdin.lock().lines() {
        let line = raw?;
        if line.trim().is_empty() {
            continue;
        }
        let message: JSONRPCMessage = serde_json::from_str(&line)?;
        if let JSONRPCMessage::Notification(notification) = message {
            let method = notification.method.clone();
            if !is_known_untyped_server_notification(&method) {
                let _typed = codex_app_server_protocol::ServerNotification::try_from(notification)
                    .map_err(|error| HarnessServerError::InvalidServerNotification {
                        message: error.to_string(),
                    })?;
            }
        }
    }
    Ok(())
}

pub(crate) fn run_blocks_app_server<H: HarnessServer>(harness: &H) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut state = initial_blocks_thread_state(harness)?;
    let mut blocks_state = BlocksState::default();
    let (_request_tx, request_rx) = mpsc::channel();

    for raw in stdin.lock().lines() {
        let line = raw?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match parse_blocks_line_with_state(trimmed, &mut blocks_state) {
            Ok(BlocksCommand::User {
                input,
                client_user_message_id,
                model,
            }) => {
                if let Some(model) = model {
                    state.model = model;
                }
                if let Err(error) = run_blocks_turn(
                    harness,
                    &mut state,
                    input,
                    client_user_message_id,
                    &mut stdout,
                    &request_rx,
                ) {
                    eprintln!("blocks turn failed: {error:#}");
                    write_blocks_error(&mut stdout, &state.id, "turn", error.to_string())?;
                }
            }
            Ok(BlocksCommand::Interrupt) => {
                eprintln!("blocks interrupt ignored: no active stdin reader while a turn runs");
            }
            Ok(BlocksCommand::AttachmentChunk) => {}
            Err(error) => {
                eprintln!("invalid blocks input: {error:#}");
                write_blocks_error(&mut stdout, &state.id, "input", error.to_string())?;
            }
        }
    }

    Ok(())
}

pub(crate) fn run_app_server<H: HarnessServer>(harness: &H) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for raw in stdin.lock().lines() {
            let Ok(line) = raw else {
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<JSONRPCMessage>(trimmed) {
                Ok(message) => message,
                Err(error) => {
                    eprintln!("invalid JSON-RPC message: {error}");
                    continue;
                }
            };
            let JSONRPCMessage::Request(request) = message else {
                continue;
            };
            if request_tx.send(request).is_err() {
                break;
            }
        }
    });

    let mut stdout = io::stdout().lock();
    let mut threads: HashMap<String, ThreadState> = HashMap::new();

    while let Ok(request) = request_rx.recv() {
        if let Err(error) = handle_request(harness, request, &request_rx, &mut threads, &mut stdout)
        {
            eprintln!("request failed: {error:#}");
        }
    }

    Ok(())
}

fn initial_blocks_thread_state<H: HarnessServer>(harness: &H) -> Result<ThreadState> {
    let cwd = env::current_dir()?;
    let params = ThreadStartParams::default();
    Ok(harness.thread_state(&params, cwd))
}

fn run_blocks_turn<H: HarnessServer, W: Write>(
    harness: &H,
    state: &mut ThreadState,
    input: Vec<UserInput>,
    client_user_message_id: Option<String>,
    stdout: &mut W,
    request_rx: &Receiver<JSONRPCRequest>,
) -> Result<()> {
    let turn_id = format!("turn-{}", Uuid::new_v4().simple());
    let mut normalizer = normalizer_for(harness, state, &turn_id);
    run_normalized_turn(
        harness,
        state,
        &input,
        client_user_message_id,
        &mut normalizer,
        stdout,
        request_rx,
    )
}

#[derive(Debug)]
pub(crate) enum BlocksCommand {
    User {
        input: Vec<UserInput>,
        client_user_message_id: Option<String>,
        model: Option<String>,
    },
    Interrupt,
    AttachmentChunk,
}

#[derive(Debug, Default)]
pub(crate) struct BlocksState {
    uploads: HashMap<String, StagedAttachment>,
    staged: HashMap<String, StagedAttachment>,
}

#[derive(Debug, Clone)]
struct StagedAttachment {
    path: PathBuf,
    mime_type: Option<String>,
    attachment_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlocksLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<BlocksMessage>,
    #[serde(default)]
    content: Option<BlocksContent>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    client_user_message_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(rename = "attachmentId", default)]
    attachment_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "mimeType", default)]
    mime_type: Option<String>,
    #[serde(rename = "attachmentType", default)]
    attachment_type: Option<String>,
    #[serde(rename = "final", default)]
    final_chunk: bool,
    #[serde(rename = "dataBase64", default)]
    data_base64: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlocksMessage {
    #[serde(default)]
    content: Option<BlocksContent>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BlocksContent {
    Inputs(Vec<BlocksInput>),
    Text(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BlocksInput {
    UserInput(UserInput),
    Attachment(AttachmentBlock),
}

#[derive(Debug, Deserialize)]
struct AttachmentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "mimeType", default)]
    mime_type: Option<String>,
    #[serde(rename = "attachment_type", default)]
    attachment_type: Option<String>,
    #[serde(rename = "stagedAttachmentId", default)]
    staged_attachment_id: Option<String>,
    #[serde(rename = "dataBase64", default)]
    data_base64: Option<String>,
    #[serde(rename = "localPath", default)]
    local_path: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "fetchError", default)]
    fetch_error: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[cfg(test)]
fn parse_blocks_line(line: &str) -> Result<BlocksCommand> {
    parse_blocks_line_with_state(line, &mut BlocksState::default())
}

pub(crate) fn parse_blocks_line_with_state(
    line: &str,
    state: &mut BlocksState,
) -> Result<BlocksCommand> {
    let parsed: BlocksLine =
        serde_json::from_str(line).map_err(|source| HarnessServerError::InvalidBlocksInput {
            message: source.to_string(),
        })?;

    match parsed.kind.as_str() {
        "user" => {
            let content = parsed
                .message
                .as_ref()
                .and_then(|message| message.content.as_ref())
                .or(parsed.content.as_ref());
            let mut input = match content {
                Some(content) => blocks_content_to_user_input(content, state)?,
                None => parsed
                    .text
                    .map(|text| {
                        vec![UserInput::Text {
                            text,
                            text_elements: Vec::new(),
                        }]
                    })
                    .unwrap_or_default(),
            };
            if input.is_empty() {
                input.push(UserInput::Text {
                    text: "continue".to_string(),
                    text_elements: Vec::new(),
                });
            }
            Ok(BlocksCommand::User {
                input,
                client_user_message_id: parsed
                    .client_user_message_id
                    .or_else(|| parsed.message.and_then(|message| message.id)),
                model: parsed
                    .model
                    .map(|model| model.trim().to_owned())
                    .filter(|model| !model.is_empty()),
            })
        }
        "attachment.chunk" => {
            handle_attachment_chunk(parsed, state)?;
            Ok(BlocksCommand::AttachmentChunk)
        }
        "interrupt" => Ok(BlocksCommand::Interrupt),
        kind => Err(HarnessServerError::InvalidBlocksInput {
            message: format!("unsupported blocks input type `{kind}`"),
        }),
    }
}

fn blocks_content_to_user_input(
    content: &BlocksContent,
    state: &mut BlocksState,
) -> Result<Vec<UserInput>> {
    match content {
        BlocksContent::Inputs(input) => input
            .iter()
            .map(|item| blocks_input_to_user_input(item, state))
            .collect::<Result<Vec<_>>>()
            .map(|items| items.into_iter().flatten().collect()),
        BlocksContent::Text(text) => Ok(vec![UserInput::Text {
            text: text.clone(),
            text_elements: Vec::new(),
        }]),
    }
}

fn blocks_input_to_user_input(
    input: &BlocksInput,
    state: &mut BlocksState,
) -> Result<Vec<UserInput>> {
    match input {
        BlocksInput::UserInput(input) => Ok(vec![input.clone()]),
        BlocksInput::Attachment(block) if block.kind == "attachment" => {
            attachment_block_to_user_input(block, state)
        }
        BlocksInput::Attachment(block) => Ok(vec![UserInput::Text {
            text: format!("[Unsupported attachment block type: {}]", block.kind),
            text_elements: Vec::new(),
        }]),
    }
}

fn attachment_block_to_user_input(
    block: &AttachmentBlock,
    state: &mut BlocksState,
) -> Result<Vec<UserInput>> {
    let name = non_empty(block.name.as_deref()).unwrap_or("attachment");
    let mime_type = non_empty(block.mime_type.as_deref());
    let attachment_type = non_empty(block.attachment_type.as_deref());

    if let Some(local_path) =
        non_empty(block.local_path.as_deref()).or(non_empty(block.path.as_deref()))
    {
        let path = PathBuf::from(local_path);
        if path.exists() {
            return Ok(local_file_inputs(
                &path,
                mime_type,
                is_image_attachment(attachment_type, mime_type),
            ));
        }
    }

    if let Some(staged_attachment_id) = non_empty(block.staged_attachment_id.as_deref()) {
        if let Some(staged) = state.staged.get(staged_attachment_id) {
            if staged.path.exists() {
                return Ok(local_file_inputs(
                    &staged.path,
                    staged.mime_type.as_deref().or(mime_type),
                    is_image_attachment(
                        staged.attachment_type.as_deref(),
                        staged.mime_type.as_deref().or(mime_type),
                    ),
                ));
            }
        }
        return Ok(vec![UserInput::Text {
            text: format!("[Attachment was not staged successfully: {name}]"),
            text_elements: Vec::new(),
        }]);
    }

    if let Some(data_base64) = non_empty(block.data_base64.as_deref()) {
        let path = write_base64_upload(data_base64, name, mime_type)?;
        return Ok(local_file_inputs(
            &path,
            mime_type,
            is_image_attachment(attachment_type, mime_type),
        ));
    }

    let mut fields = vec![format!("name={name}")];
    if let Some(mime_type) = mime_type {
        fields.push(format!("mime={mime_type}"));
    }
    if let Some(url) = non_empty(block.url.as_deref()) {
        fields.push(format!("url={url}"));
    }
    if let Some(fetch_error) = non_empty(block.fetch_error.as_deref()) {
        fields.push(format!("fetch_error={fetch_error}"));
    }
    Ok(vec![UserInput::Text {
        text: format!("[Slack attachment: {}]", fields.join(" ")),
        text_elements: Vec::new(),
    }])
}

fn handle_attachment_chunk(parsed: BlocksLine, state: &mut BlocksState) -> Result<()> {
    let attachment_id = non_empty(parsed.attachment_id.as_deref()).ok_or_else(|| {
        HarnessServerError::InvalidBlocksInput {
            message: "attachment chunk missing attachmentId".to_string(),
        }
    })?;
    let name = non_empty(parsed.name.as_deref()).unwrap_or("attachment");
    let mime_type = non_empty(parsed.mime_type.as_deref());

    if !state.uploads.contains_key(attachment_id) {
        let path = unique_upload_path(name, mime_type)?;
        state.uploads.insert(
            attachment_id.to_string(),
            StagedAttachment {
                path,
                mime_type: mime_type.map(ToOwned::to_owned),
                attachment_type: non_empty(parsed.attachment_type.as_deref())
                    .map(ToOwned::to_owned),
            },
        );
    }

    if let Some(data_base64) = non_empty(parsed.data_base64.as_deref()) {
        let bytes = BASE64_STANDARD.decode(data_base64).map_err(|source| {
            HarnessServerError::InvalidBlocksInput {
                message: format!("invalid attachment chunk for {attachment_id}: {source}"),
            }
        })?;
        let upload = state.uploads.get(attachment_id).expect("upload exists");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&upload.path)?;
        file.write_all(&bytes)?;
    }

    if parsed.final_chunk {
        if let Some(upload) = state.uploads.remove(attachment_id) {
            state.staged.insert(attachment_id.to_string(), upload);
        }
    }

    Ok(())
}

fn local_file_inputs(path: &Path, mime_type: Option<&str>, is_image: bool) -> Vec<UserInput> {
    let display_path = path.display();
    if is_image || mime_type.is_some_and(|value| value.starts_with("image/")) {
        return vec![
            UserInput::Text {
                text: format!("[Attached image saved to {display_path}]"),
                text_elements: Vec::new(),
            },
            UserInput::LocalImage {
                path: path.to_path_buf(),
                detail: None,
            },
        ];
    }
    vec![UserInput::Text {
        text: format!("[Attached file saved to {display_path}]"),
        text_elements: Vec::new(),
    }]
}

fn write_base64_upload(data_base64: &str, name: &str, mime_type: Option<&str>) -> Result<PathBuf> {
    let bytes = BASE64_STANDARD.decode(data_base64).map_err(|source| {
        HarnessServerError::InvalidBlocksInput {
            message: format!("invalid attachment dataBase64: {source}"),
        }
    })?;
    let path = unique_upload_path(name, mime_type)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn unique_upload_path(name: &str, mime_type: Option<&str>) -> Result<PathBuf> {
    let uploads_dir = uploads_dir();
    std::fs::create_dir_all(&uploads_dir)?;
    let mut base = sanitize_upload_name(name);
    if Path::new(&base).extension().is_none()
        && let Some(extension) = extension_for_mime_type(mime_type)
    {
        base.push_str(extension);
    }
    let path = uploads_dir.join(&base);
    if !path.exists() {
        return Ok(path);
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let suffix = if suffix.is_empty() {
        String::new()
    } else {
        format!(".{suffix}")
    };
    Ok(uploads_dir.join(format!("{stem}-{}{suffix}", Uuid::new_v4().simple())))
}

fn uploads_dir() -> PathBuf {
    env::var_os("CENTAUR_UPLOADS_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join("uploads")))
        .unwrap_or_else(|| PathBuf::from("/tmp/uploads"))
}

fn sanitize_upload_name(name: &str) -> String {
    let leaf = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment")
        .trim();
    let clean = leaf
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_'])
        .to_string();
    if clean.is_empty() {
        "attachment".to_string()
    } else {
        clean
    }
}

fn extension_for_mime_type(mime_type: Option<&str>) -> Option<&'static str> {
    match mime_type? {
        "image/jpeg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "video/mp4" => Some(".mp4"),
        "application/pdf" => Some(".pdf"),
        "text/plain" => Some(".txt"),
        _ => None,
    }
}

fn is_image_attachment(attachment_type: Option<&str>, mime_type: Option<&str>) -> bool {
    attachment_type == Some("image") || mime_type.is_some_and(|value| value.starts_with("image/"))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn handle_request<H: HarnessServer, W: Write>(
    harness: &H,
    request: JSONRPCRequest,
    request_rx: &Receiver<JSONRPCRequest>,
    threads: &mut HashMap<String, ThreadState>,
    stdout: &mut W,
) -> Result<()> {
    match request.method.as_str() {
        "initialize" => {
            let response = InitializeResponse {
                user_agent: "harness-server".to_string(),
                codex_home: absolute_path(
                    env::var_os("CODEX_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(default_codex_home),
                )?,
                platform_family: env::consts::FAMILY.to_string(),
                platform_os: env::consts::OS.to_string(),
            };
            write_client_response(
                stdout,
                ClientResponse::Initialize {
                    request_id: request.id,
                    response,
                },
            )
        }
        "thread/start" => {
            let params: ThreadStartParams = request_params(request.params)?;
            let cwd = params
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or(env::current_dir()?);
            let cwd = if cwd.is_absolute() {
                cwd
            } else {
                env::current_dir()?.join(cwd)
            };
            let state = harness.thread_state(&params, cwd.clone());
            let thread_id = state.id.clone();
            let normalizer = normalizer_for(harness, &state, "turn-placeholder");
            let response = ThreadStartResponse {
                thread: normalizer.thread_snapshot()?,
                model: state.model.clone(),
                model_provider: state.model_provider.clone(),
                service_tier: state.service_tier.clone(),
                cwd: absolute_path(cwd)?,
                runtime_workspace_roots: Vec::new(),
                instruction_sources: Vec::new(),
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::DangerFullAccess,
                active_permission_profile: None,
                reasoning_effort: None,
            };
            threads.insert(thread_id, state);
            write_client_response(
                stdout,
                ClientResponse::ThreadStart {
                    request_id: request.id,
                    response,
                },
            )
        }
        "thread/resume" => {
            let params: ThreadResumeParams = request_params(request.params)?;
            let thread_id = params.thread_id.clone();
            if !threads.contains_key(&thread_id) {
                threads.insert(thread_id.clone(), resumed_thread_state(harness, &params)?);
            }
            let state = threads
                .get_mut(&thread_id)
                .expect("thread state inserted or existed");
            apply_resume_overrides(state, &params)?;
            let normalizer = normalizer_for(harness, &state, "turn-placeholder");
            let mut thread = normalizer.thread_snapshot()?;
            if !params.exclude_turns {
                thread.turns = state.completed_turns.clone();
            }
            let response = ThreadResumeResponse {
                thread,
                model: state.model.clone(),
                model_provider: state.model_provider.clone(),
                service_tier: state.service_tier.clone(),
                cwd: absolute_path(state.cwd.clone())?,
                runtime_workspace_roots: Vec::new(),
                instruction_sources: Vec::new(),
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::DangerFullAccess,
                active_permission_profile: None,
                reasoning_effort: None,
                initial_turns_page: None,
            };
            write_client_response(
                stdout,
                ClientResponse::ThreadResume {
                    request_id: request.id,
                    response,
                },
            )
        }
        "turn/start" => {
            let params: TurnStartParams = request_params(request.params)?;
            let state = threads.get_mut(&params.thread_id).ok_or_else(|| {
                HarnessServerError::UnknownThread {
                    thread_id: params.thread_id.clone(),
                }
            })?;
            let turn_id = format!("turn-{}", Uuid::new_v4().simple());
            let mut normalizer = normalizer_for(harness, state, &turn_id);
            let response = TurnStartResponse {
                turn: normalizer.turn_snapshot(TurnStatus::InProgress),
            };
            write_client_response(
                stdout,
                ClientResponse::TurnStart {
                    request_id: request.id,
                    response,
                },
            )?;
            run_normalized_turn(
                harness,
                state,
                &params.input,
                params.client_user_message_id.clone(),
                &mut normalizer,
                stdout,
                request_rx,
            )
        }
        "turn/interrupt" => {
            let _params: TurnInterruptParams = request_params(request.params)?;
            write_client_response(
                stdout,
                ClientResponse::TurnInterrupt {
                    request_id: request.id,
                    response: TurnInterruptResponse {},
                },
            )
        }
        "turn/steer" => write_error(
            stdout,
            request.id,
            -32600,
            "no active turn to steer".to_string(),
        ),
        _ => write_error(
            stdout,
            request.id,
            -32601,
            format!("method not found: {}", request.method),
        ),
    }
}

fn normalizer_for<H: HarnessServer>(
    harness: &H,
    state: &ThreadState,
    turn_id: &str,
) -> CodexTurnNormalizer {
    let mut config = BridgeConfig::new(state.id.clone(), turn_id.to_string());
    config.cwd = state.cwd.clone();
    config.cli_version = harness.cli_version().to_string();
    config.model_provider = state.model_provider.clone();
    CodexTurnNormalizer::new(config)
}

fn resumed_thread_state<H: HarnessServer>(
    harness: &H,
    params: &ThreadResumeParams,
) -> Result<ThreadState> {
    let cwd = params
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        env::current_dir()?.join(cwd)
    };
    Ok(ThreadState {
        id: params.thread_id.clone(),
        cwd,
        model: params
            .model
            .clone()
            .unwrap_or_else(|| harness.default_model()),
        model_provider: params
            .model_provider
            .clone()
            .unwrap_or_else(|| harness.default_model_provider().to_string()),
        service_tier: params.service_tier.clone().flatten(),
        harness_session_id: Some(params.thread_id.clone()),
        completed_turns: Vec::new(),
        process: None,
        thread_started_sent: false,
    })
}

fn apply_resume_overrides(state: &mut ThreadState, params: &ThreadResumeParams) -> Result<()> {
    if let Some(model) = &params.model {
        state.model = model.clone();
    }
    if let Some(model_provider) = &params.model_provider {
        state.model_provider = model_provider.clone();
    }
    if let Some(service_tier) = &params.service_tier {
        state.service_tier.clone_from(service_tier);
    }
    if let Some(cwd) = &params.cwd {
        let cwd = PathBuf::from(cwd);
        state.cwd = if cwd.is_absolute() {
            cwd
        } else {
            env::current_dir()?.join(cwd)
        };
    }
    Ok(())
}

fn handle_active_turn_request<H: HarnessServer, W: Write>(
    harness: &H,
    process: &mut HarnessChild,
    normalizer: &mut CodexTurnNormalizer,
    request: JSONRPCRequest,
    stdout: &mut W,
) -> Result<()> {
    match request.method.as_str() {
        "turn/steer" => {
            let params: TurnSteerParams = request_params(request.params)?;
            if params.thread_id != normalizer.thread_id() {
                write_error(
                    stdout,
                    request.id,
                    -32600,
                    format!("unknown threadId {}", params.thread_id),
                )?;
                return Ok(());
            }
            if params.expected_turn_id != normalizer.turn_id() {
                write_error(
                    stdout,
                    request.id,
                    -32600,
                    format!(
                        "expected active turn id `{}` but found `{}`",
                        params.expected_turn_id,
                        normalizer.turn_id()
                    ),
                )?;
                return Ok(());
            }
            process
                .stdin
                .write_all(&harness.stdin_for_steer(&params.input)?)?;
            process.stdin.flush()?;
            write_client_response(
                stdout,
                ClientResponse::TurnSteer {
                    request_id: request.id,
                    response: TurnSteerResponse {
                        turn_id: normalizer.turn_id().to_string(),
                    },
                },
            )?;
            for notification in normalizer
                .emit_user_message(params.client_user_message_id.clone(), params.input.clone())?
            {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            Ok(())
        }
        "turn/interrupt" => {
            write_client_response(
                stdout,
                ClientResponse::TurnInterrupt {
                    request_id: request.id,
                    response: TurnInterruptResponse {},
                },
            )?;
            Ok(())
        }
        _ => {
            write_error(
                stdout,
                request.id,
                -32600,
                format!("cannot handle {} while a turn is active", request.method),
            )?;
            Ok(())
        }
    }
}

fn run_normalized_turn<H: HarnessServer, W: Write>(
    harness: &H,
    state: &mut ThreadState,
    input: &[UserInput],
    client_user_message_id: Option<String>,
    normalizer: &mut CodexTurnNormalizer,
    stdout: &mut W,
    request_rx: &Receiver<JSONRPCRequest>,
) -> Result<()> {
    for notification in normalizer.start_notifications(!state.thread_started_sent)? {
        if matches!(notification, ServerNotification::ThreadStarted(_)) {
            state.thread_started_sent = true;
        }
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    for notification in normalizer.emit_user_message(client_user_message_id, input.to_vec())? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }

    match run_harness_turn(harness, state, input, normalizer, stdout, request_rx) {
        Ok(Some(turn)) => state.completed_turns.push(turn),
        Ok(None) => {}
        Err(error) => finish_turn_with_error(state, normalizer, stdout, error)?,
    }
    Ok(())
}

fn finish_turn_with_error<W: Write>(
    state: &mut ThreadState,
    normalizer: &mut CodexTurnNormalizer,
    stdout: &mut W,
    error: HarnessServerError,
) -> Result<()> {
    let message = error.to_string();
    let normalized = NormalizedEvent::Error {
        message: message.clone(),
    };
    for notification in normalizer.process_event(&normalized)? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    if let Some(notification) = normalizer.finish_turn(Some(message))? {
        if let ServerNotification::TurnCompleted(completed) = &notification {
            state.completed_turns.push(completed.turn.clone());
        }
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    Ok(())
}

fn run_harness_turn<H: HarnessServer, W: Write>(
    harness: &H,
    state: &mut ThreadState,
    input: &[UserInput],
    normalizer: &mut CodexTurnNormalizer,
    stdout: &mut W,
    request_rx: &Receiver<JSONRPCRequest>,
) -> Result<Option<codex_app_server_protocol::Turn>> {
    ensure_harness_process(harness, state)?;
    let process = state
        .process
        .as_mut()
        .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
    process.stdin.write_all(&harness.stdin_for_turn(input)?)?;
    process.stdin.flush()?;

    let mut last_session_id = state.harness_session_id.clone();
    let mut event_normalizer = H::EventNormalizer::default();
    let mut completed_turn = None;
    loop {
        while let Ok(request) = request_rx.try_recv() {
            handle_active_turn_request(harness, process, normalizer, request, stdout)?;
        }

        let line = match process.stdout.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let status = process.child.wait()?;
                return Err(HarnessServerError::HarnessExited {
                    kind: harness.kind(),
                    status,
                    stderr: String::new(),
                });
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = harness.parse_stdout_line(trimmed)?;
        let normalized_events = harness.normalize_events(&mut event_normalizer, event)?;
        let mut terminal = false;
        for normalized in normalized_events {
            if let Some(session_id) = normalized.session_id() {
                last_session_id = Some(session_id.to_string());
                state.harness_session_id = Some(session_id.to_string());
            }
            for notification in normalizer.process_event(&normalized)? {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            terminal |= normalized.is_terminal()
                || (harness.finish_turn_on_assistant_end_turn()
                    && normalized.is_assistant_end_turn());
        }
        if terminal {
            if let Some(notification) = normalizer.finish_turn(None)? {
                if let ServerNotification::TurnCompleted(completed) = &notification {
                    completed_turn = Some(completed.turn.clone());
                }
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            break;
        }
    }

    if let Some(session_id) = last_session_id {
        state.harness_session_id = Some(session_id);
    }
    if let Some(notification) = normalizer.finish_turn(None)? {
        if let ServerNotification::TurnCompleted(completed) = &notification {
            completed_turn = Some(completed.turn.clone());
        }
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    Ok(completed_turn)
}

fn ensure_harness_process<H: HarnessServer>(harness: &H, state: &mut ThreadState) -> Result<()> {
    if state.process.is_some() {
        return Ok(());
    }

    let mut command = harness.command_for_turn(state);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&state.cwd)
        .spawn()
        .map_err(|source| HarnessServerError::SpawnHarness {
            cwd: state.cwd.clone(),
            source,
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(HarnessServerError::HarnessStdoutUnavailable)?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(HarnessServerError::HarnessStderrUnavailable)?;
    std::thread::spawn(move || {
        let mut parent_stderr = io::stderr().lock();
        let _ = io::copy(&mut stderr, &mut parent_stderr);
    });
    let (stdout_tx, stdout_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = io::BufReader::new(stdout);
        for raw in reader.lines() {
            let should_stop = raw.is_err();
            if stdout_tx.send(raw).is_err() || should_stop {
                break;
            }
        }
    });

    state.process = Some(HarnessChild {
        child,
        stdin,
        stdout: stdout_rx,
    });
    Ok(())
}

fn request_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T> {
    serde_json::from_value(params.unwrap_or_else(|| json!({})))
        .map_err(|source| HarnessServerError::InvalidParams { source })
}

fn write_client_response<W: Write>(stdout: &mut W, response: ClientResponse) -> Result<()> {
    let (id, result) = response.into_jsonrpc_parts()?;
    write_value(
        stdout,
        &serde_json::to_value(JSONRPCMessage::Response(JSONRPCResponse { id, result }))?,
    )
}

fn write_error<W: Write>(stdout: &mut W, id: RequestId, code: i64, message: String) -> Result<()> {
    write_value(
        stdout,
        &serde_json::to_value(JSONRPCMessage::Error(JSONRPCError {
            id,
            error: JSONRPCErrorError {
                code,
                message,
                data: None,
            },
        }))?,
    )
}

pub(crate) fn write_blocks_error<W: Write>(
    stdout: &mut W,
    thread_id: &str,
    turn_id: &str,
    message: String,
) -> Result<()> {
    write_value(
        stdout,
        &json!({
            "method": "error",
            "params": {
                "error": {
                    "message": message,
                    "codexErrorInfo": null,
                    "additionalDetails": null
                },
                "willRetry": false,
                "threadId": thread_id,
                "turnId": turn_id
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_upload_dir() -> PathBuf {
        let path = env::temp_dir().join(format!("harness-server-test-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&path).expect("create temp upload dir");
        unsafe {
            env::set_var("CENTAUR_UPLOADS_DIR", &path);
        }
        path
    }

    #[test]
    fn parses_blocks_user_line_with_model_override() {
        let line = r#"{"type":"user","thread_key":"web:t1","model":"claude-sonnet-4-6","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        let BlocksCommand::User { model, input, .. } = parse_blocks_line(line).expect("parses")
        else {
            panic!("expected user command");
        };
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn ignores_blank_model_on_blocks_user_line() {
        let line = r#"{"type":"user","model":"  ","text":"hi"}"#;
        let BlocksCommand::User { model, .. } = parse_blocks_line(line).expect("parses") else {
            panic!("expected user command");
        };
        assert_eq!(model, None);
    }

    #[test]
    fn parses_attachment_chunk_without_starting_turn() {
        let _upload_dir = temp_upload_dir();
        let mut state = BlocksState::default();
        let line = r#"{"type":"attachment.chunk","attachmentId":"att-1","name":"large-upload.mp4","mimeType":"video/mp4","attachmentType":"video","chunkIndex":0,"final":true,"dataBase64":"aGVsbG8="}"#;

        assert!(matches!(
            parse_blocks_line_with_state(line, &mut state).expect("parses"),
            BlocksCommand::AttachmentChunk
        ));

        let staged = state.staged.get("att-1").expect("staged attachment");
        assert_eq!(staged.mime_type.as_deref(), Some("video/mp4"));
        assert_eq!(
            std::fs::read(&staged.path).expect("read staged bytes"),
            b"hello"
        );
    }

    #[test]
    fn staged_attachment_block_becomes_local_file_text_input() {
        let _upload_dir = temp_upload_dir();
        let mut state = BlocksState::default();
        let chunk = r#"{"type":"attachment.chunk","attachmentId":"att-1","name":"clip.mp4","mimeType":"video/mp4","attachmentType":"video","chunkIndex":0,"final":true,"dataBase64":"aGVsbG8="}"#;
        parse_blocks_line_with_state(chunk, &mut state).expect("chunk parses");

        let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"analyze this"},{"type":"attachment","attachment_type":"video","stagedAttachmentId":"att-1","name":"clip.mp4","mimeType":"video/mp4","size":5}]}}"#;
        let BlocksCommand::User { input, .. } =
            parse_blocks_line_with_state(user, &mut state).expect("user parses")
        else {
            panic!("expected user command");
        };

        assert_eq!(input.len(), 2);
        assert_eq!(
            input[0],
            UserInput::Text {
                text: "analyze this".to_string(),
                text_elements: Vec::new()
            }
        );
        let UserInput::Text { text, .. } = &input[1] else {
            panic!("expected staged attachment to become text");
        };
        assert!(text.starts_with("[Attached file saved to "));
        assert!(text.ends_with("clip.mp4]"));
    }

    #[test]
    fn inline_attachment_block_becomes_local_file_text_input() {
        let _upload_dir = temp_upload_dir();
        let mut state = BlocksState::default();
        let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"attachment","attachment_type":"document","dataBase64":"aGVsbG8=","name":"notes.txt","mimeType":"text/plain","size":5}]}}"#;
        let BlocksCommand::User { input, .. } =
            parse_blocks_line_with_state(user, &mut state).expect("user parses")
        else {
            panic!("expected user command");
        };

        assert_eq!(input.len(), 1);
        let UserInput::Text { text, .. } = &input[0] else {
            panic!("expected inline attachment to become text");
        };
        assert!(text.starts_with("[Attached file saved to "));
        assert!(text.ends_with("notes.txt]"));
    }
}
