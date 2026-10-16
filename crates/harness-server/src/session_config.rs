//! Late binding of a sandbox's harness and persona.
//!
//! Warm-pool sandboxes boot before any session claims them, so they cannot
//! know which harness or persona the session will use. They run
//! `harness-server deferred`, which blocks on the first stdin line: a
//! `session.configure` message naming the harness and, optionally, the
//! persona. Once applied, the process dispatches to that harness's normal
//! blocks server and never rebinds.
//!
//! The control plane resends `session.configure` every time it opens a pipe to
//! a deferred sandbox, because it cannot know whether the harness-server it
//! reaches has already been bound (a control-plane restart reattaches to a
//! bound process; a resumed pod restarts an unbound one). Every blocks parser
//! therefore accepts a repeated `session.configure`: it is a no-op when it
//! matches the binding and an input error when it does not. Processes launched
//! with an explicit harness subcommand have no binding and ignore it.

use std::env;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::traits::HarnessKind;
use crate::{HarnessServerError, Result};

pub(crate) const SESSION_CONFIGURE_TYPE: &str = "session.configure";
const PERSONA_PROMPT_FILE: &str = "AGENTS_PERSONA.md";
const COMPOSED_PROMPT_FILE: &str = "AGENTS.md";
const AMP_PROMPT_LINK: &str = "AGENT.md";
const COMPOSE_SYSTEM_PROMPT_BIN_ENV: &str = "CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN";
const DEFAULT_COMPOSE_SYSTEM_PROMPT_BIN: &str = "compose-system-prompt";

static BINDING: OnceLock<SessionBinding> = OnceLock::new();

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionConfigure {
    harness: String,
    #[serde(default)]
    persona: Option<PersonaConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct PersonaConfig {
    id: String,
    prompt: String,
    #[serde(default)]
    prompt_hash: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    source_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredHarness {
    Blocks(HarnessKind),
    Nanocodex,
    Hermes,
}

impl DeferredHarness {
    /// Accepts the harness-server subcommand names (what api-rs passes as
    /// container args) plus the CLI's `claude` alias.
    fn parse(name: &str) -> Result<Self> {
        match name.trim() {
            "codex" => Ok(Self::Blocks(HarnessKind::Codex)),
            "claude-code" | "claude" => Ok(Self::Blocks(HarnessKind::ClaudeCode)),
            "amp" => Ok(Self::Blocks(HarnessKind::Amp)),
            "pi" => Ok(Self::Blocks(HarnessKind::Pi)),
            "omp" => Ok(Self::Blocks(HarnessKind::Omp)),
            "nanocodex" => Ok(Self::Nanocodex),
            "hermes" => Ok(Self::Hermes),
            other => Err(configure_error(format!("unknown harness `{other}`"))),
        }
    }
}

/// What a deferred process was bound to. Persona identity is the id plus the
/// prompt hash, so a changed prompt under the same id is also a mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionBinding {
    harness: DeferredHarness,
    persona_id: Option<String>,
    persona_prompt_hash: Option<String>,
}

impl SessionBinding {
    fn from_configure(configure: &SessionConfigure) -> Result<Self> {
        Ok(Self {
            harness: DeferredHarness::parse(&configure.harness)?,
            persona_id: configure.persona.as_ref().map(|persona| persona.id.clone()),
            persona_prompt_hash: configure
                .persona
                .as_ref()
                .and_then(|persona| persona.prompt_hash.clone()),
        })
    }
}

/// Wait for `session.configure`, apply it, then serve the bound harness.
pub fn run_deferred_server() -> Result<()> {
    let configure = read_initial_configure(&mut io::stdin().lock())?;
    let binding = SessionBinding::from_configure(&configure)?;
    let home = home_dir()?;
    let cwd = env::current_dir()?;
    apply_session_configure(&configure, binding.harness, &home, &cwd)?;
    eprintln!(
        "harness-server: bound deferred sandbox to harness `{}`{}",
        configure.harness.trim(),
        binding
            .persona_id
            .as_deref()
            .map(|id| format!(" with persona `{id}`"))
            .unwrap_or_default(),
    );
    let harness = binding.harness;
    // Only set once, here, before any harness server starts reading stdin.
    let _ = BINDING.set(binding);

    // `io::stdin()` is one process-wide buffered reader, so whatever followed
    // the configure line is still buffered for the harness server.
    match harness {
        DeferredHarness::Blocks(kind) => crate::server::run_blocks_server(kind),
        DeferredHarness::Nanocodex => crate::nanocodex::run_nanocodex_blocks_server(),
        DeferredHarness::Hermes => crate::hermes::run_hermes_blocks_server(),
    }
}

/// Validate a `session.configure` that reached an already-running harness
/// server. Matching resends are no-ops.
pub(crate) fn check_repeated_configure(line: &str) -> Result<()> {
    let configure = parse_configure(line)?;
    let Some(bound) = BINDING.get() else {
        // Launched with an explicit harness subcommand; the container args are
        // the binding and the control plane already chose them.
        return Ok(());
    };
    let requested = SessionBinding::from_configure(&configure)?;
    if &requested == bound {
        return Ok(());
    }
    Err(configure_error(format!(
        "sandbox is already bound to {bound:?}; refusing to rebind to {requested:?}"
    )))
}

fn read_initial_configure(reader: &mut impl BufRead) -> Result<SessionConfigure> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(configure_error(
                "stdin closed before session.configure arrived".to_owned(),
            ));
        }
        if !line.trim().is_empty() {
            return parse_configure(line.trim());
        }
    }
}

fn parse_configure(line: &str) -> Result<SessionConfigure> {
    #[derive(Deserialize)]
    struct Kind {
        #[serde(rename = "type", default)]
        kind: String,
    }
    let Kind { kind } = serde_json::from_str(line)
        .map_err(|source| configure_error(format!("invalid input line: {source}")))?;
    if kind != SESSION_CONFIGURE_TYPE {
        return Err(configure_error(format!(
            "expected `{SESSION_CONFIGURE_TYPE}` before any other input, got `{kind}`"
        )));
    }
    serde_json::from_str(line)
        .map_err(|source| configure_error(format!("invalid session.configure line: {source}")))
}

fn apply_session_configure(
    configure: &SessionConfigure,
    harness: DeferredHarness,
    home: &Path,
    cwd: &Path,
) -> Result<()> {
    if let Some(persona) = &configure.persona {
        fs::write(home.join(PERSONA_PROMPT_FILE), &persona.prompt)?;
        compose_system_prompt(home, cwd)?;
        set_persona_env(persona);
    } else if home.join(PERSONA_PROMPT_FILE).exists() {
        fs::remove_file(home.join(PERSONA_PROMPT_FILE))?;
        compose_system_prompt(home, cwd)?;
    }
    if harness == DeferredHarness::Blocks(HarnessKind::Amp) {
        link_amp_prompt(cwd)?;
    }
    Ok(())
}

/// Recompose the workspace prompt the same way the entrypoint does at boot,
/// now with the persona prompt present.
fn compose_system_prompt(home: &Path, cwd: &Path) -> Result<()> {
    let bin = env::var(COMPOSE_SYSTEM_PROMPT_BIN_ENV)
        .ok()
        .filter(|bin| !bin.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_COMPOSE_SYSTEM_PROMPT_BIN.to_owned());
    let status = ProcessCommand::new(&bin)
        .arg("--home-dir")
        .arg(home)
        .arg("--target-prompt")
        .arg(cwd.join(COMPOSED_PROMPT_FILE))
        .status()
        .map_err(|source| configure_error(format!("failed to run {bin}: {source}")))?;
    if !status.success() {
        return Err(configure_error(format!("{bin} exited with {status}")));
    }
    Ok(())
}

/// Amp reads `AGENT.md`; the entrypoint links it for `harness-server amp`.
fn link_amp_prompt(cwd: &Path) -> Result<()> {
    if !cwd.join(COMPOSED_PROMPT_FILE).is_file() {
        return Ok(());
    }
    let link = cwd.join(AMP_PROMPT_LINK);
    match fs::remove_file(&link) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::os::unix::fs::symlink(COMPOSED_PROMPT_FILE, link)?;
    Ok(())
}

/// Mirror the persona env a cold sandbox gets from its pod spec, so harness
/// and tool processes spawned later see the same values.
fn set_persona_env(persona: &PersonaConfig) {
    let vars = [
        ("AGENT_PERSONA", Some(persona.id.as_str())),
        ("CENTAUR_PERSONA_ID", Some(persona.id.as_str())),
        (
            "CENTAUR_PERSONA_PROMPT_HASH",
            persona.prompt_hash.as_deref(),
        ),
        (
            "CENTAUR_PERSONA_SOURCE_PATH",
            persona.source_path.as_deref(),
        ),
        ("CENTAUR_PERSONA_SOURCE_REF", persona.source_ref.as_deref()),
    ];
    for (name, value) in vars {
        // SAFETY: called once on the main thread before any harness server,
        // telemetry exporter, or child process starts. The only other thread
        // is the SIGTERM watcher, which is parked in its signal future and
        // never reads the environment.
        unsafe {
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or(HarnessServerError::MissingEnvironment { name: "HOME" })
}

fn configure_error(message: String) -> HarnessServerError {
    HarnessServerError::SessionConfigure { message }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn configure(harness: &str, persona: Option<(&str, &str)>) -> SessionConfigure {
        SessionConfigure {
            harness: harness.to_owned(),
            persona: persona.map(|(id, hash)| PersonaConfig {
                id: id.to_owned(),
                prompt: format!("{id} persona prompt"),
                prompt_hash: Some(hash.to_owned()),
                source_path: Some(format!("/repo/personas/{id}")),
                source_ref: None,
            }),
        }
    }

    #[test]
    fn initial_configure_skips_blank_lines_and_leaves_the_rest_buffered() {
        let input = concat!(
            "\n",
            r#"{"type":"session.configure","harness":"pi","persona":{"id":"eng","prompt":"be terse","prompt_hash":"sha256:1"}}"#,
            "\n",
            r#"{"type":"user","text":"hi"}"#,
            "\n",
        );
        let mut reader = Cursor::new(input.as_bytes());

        let parsed = read_initial_configure(&mut reader).expect("configure parses");

        assert_eq!(parsed.harness, "pi");
        assert_eq!(parsed.persona.as_ref().unwrap().id, "eng");
        let mut rest = String::new();
        reader.read_line(&mut rest).unwrap();
        assert_eq!(rest.trim(), r#"{"type":"user","text":"hi"}"#);
    }

    #[test]
    fn initial_input_must_be_configure() {
        let mut reader = Cursor::new(br#"{"type":"user","text":"hi"}"#.to_vec());
        let error = read_initial_configure(&mut reader).unwrap_err();
        assert!(error.to_string().contains("expected `session.configure`"));

        let mut closed = Cursor::new(Vec::new());
        let error = read_initial_configure(&mut closed).unwrap_err();
        assert!(error.to_string().contains("stdin closed"));
    }

    #[test]
    fn harness_names_match_the_server_subcommands() {
        for (name, expected) in [
            ("codex", DeferredHarness::Blocks(HarnessKind::Codex)),
            (
                "claude-code",
                DeferredHarness::Blocks(HarnessKind::ClaudeCode),
            ),
            ("claude", DeferredHarness::Blocks(HarnessKind::ClaudeCode)),
            ("amp", DeferredHarness::Blocks(HarnessKind::Amp)),
            ("pi", DeferredHarness::Blocks(HarnessKind::Pi)),
            ("omp", DeferredHarness::Blocks(HarnessKind::Omp)),
            ("nanocodex", DeferredHarness::Nanocodex),
            ("hermes", DeferredHarness::Hermes),
        ] {
            assert_eq!(DeferredHarness::parse(name).unwrap(), expected, "{name}");
        }
        assert!(DeferredHarness::parse("cursor").is_err());
    }

    #[test]
    fn binding_compares_harness_and_persona_identity() {
        let bound = SessionBinding::from_configure(&configure("pi", Some(("eng", "h1")))).unwrap();
        let same = SessionBinding::from_configure(&configure("pi", Some(("eng", "h1")))).unwrap();
        assert_eq!(bound, same);
        for other in [
            configure("codex", Some(("eng", "h1"))),
            configure("pi", None),
            configure("pi", Some(("ops", "h1"))),
            configure("pi", Some(("eng", "h2"))),
        ] {
            assert_ne!(bound, SessionBinding::from_configure(&other).unwrap());
        }
    }

    #[test]
    fn unbound_process_accepts_any_configure_but_rejects_other_input() {
        // Tests never call run_deferred_server, so BINDING stays unset.
        check_repeated_configure(r#"{"type":"session.configure","harness":"amp"}"#)
            .expect("unbound processes ignore configure");
        assert!(check_repeated_configure(r#"{"type":"session.configure"}"#).is_err());
    }

    #[test]
    fn persona_configure_writes_prompt_recomposes_and_links_amp() {
        let dir = temp_dir("persona");
        let home = dir.join("home");
        let workspace = dir.join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        // Stand-in for compose-system-prompt: concatenates the persona prompt
        // into the target, proving the args it received.
        let compose = dir.join("compose");
        fs::write(
            &compose,
            "#!/bin/sh\nset -e\nwhile [ $# -gt 0 ]; do case \"$1\" in --home-dir) home=\"$2\"; shift 2;; --target-prompt) target=\"$2\"; shift 2;; *) exit 9;; esac; done\nprintf 'base\\n' > \"$target\"\ncat \"$home/AGENTS_PERSONA.md\" >> \"$target\"\n",
        )
        .unwrap();
        fs::set_permissions(&compose, fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: test-only override; no other test reads this variable.
        unsafe { env::set_var(COMPOSE_SYSTEM_PROMPT_BIN_ENV, &compose) };

        let configure = configure("amp", Some(("eng", "h1")));
        apply_session_configure(
            &configure,
            DeferredHarness::Blocks(HarnessKind::Amp),
            &home,
            &workspace,
        )
        .expect("apply configure");

        assert_eq!(
            fs::read_to_string(home.join(PERSONA_PROMPT_FILE)).unwrap(),
            "eng persona prompt"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(COMPOSED_PROMPT_FILE)).unwrap(),
            "base\neng persona prompt"
        );
        assert_eq!(
            fs::read_link(workspace.join(AMP_PROMPT_LINK)).unwrap(),
            PathBuf::from(COMPOSED_PROMPT_FILE)
        );
        assert_eq!(env::var("CENTAUR_PERSONA_ID").as_deref(), Ok("eng"));
        assert_eq!(env::var("CENTAUR_PERSONA_PROMPT_HASH").as_deref(), Ok("h1"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn personaless_configure_leaves_the_boot_prompt_alone() {
        let dir = temp_dir("no-persona");
        let home = dir.join("home");
        let workspace = dir.join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join(COMPOSED_PROMPT_FILE), "boot prompt").unwrap();

        apply_session_configure(
            &configure("codex", None),
            DeferredHarness::Blocks(HarnessKind::Codex),
            &home,
            &workspace,
        )
        .expect("apply configure");

        assert_eq!(
            fs::read_to_string(workspace.join(COMPOSED_PROMPT_FILE)).unwrap(),
            "boot prompt"
        );
        assert!(!workspace.join(AMP_PROMPT_LINK).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "harness-server-session-config-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
