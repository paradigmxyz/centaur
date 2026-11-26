//! The instructions a thread's model sees after nanocode's own system prompt.
//!
//! They are, in order: the brain's base prompt, which always applies; with
//! Code Mode, the brain's connector guidance and the Code Mode tool
//! instructions; and the deployment's custom system prompt, set in the Console,
//! when it is not empty. The custom prompt is last because it is the most
//! specific. Each part is separated by a horizontal rule, as the sandbox
//! composes its prompt.

/// Always applied.
pub const BASE: &str = include_str!("prompts/base.md");

/// Applied with Code Mode, before the Code Mode tool instructions.
pub const CODE_MODE: &str = include_str!("prompts/code_mode.md");

const SEPARATOR: &str = "\n\n---\n\n";

/// The Console's custom system prompt, normalized: `None` when it is unset or
/// blank, so an empty prompt adds nothing.
pub fn custom(prompt: Option<&str>) -> Option<String> {
    prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(str::to_owned)
}

/// The instructions for a thread.
pub fn compose(code_mode: bool, custom: Option<&str>) -> String {
    let mut parts = vec![BASE.trim()];
    if code_mode {
        parts.push(CODE_MODE.trim());
        parts.push(centaur_codemode_protocol::tools::INSTRUCTIONS);
    }
    if let Some(custom) = custom {
        parts.push(custom);
    }
    parts.join(SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_custom_prompts_add_nothing() {
        for blank in [None, Some(""), Some("  \n\t")] {
            assert_eq!(custom(blank), None);
        }
        assert_eq!(custom(Some("  Be terse.\n")), Some("Be terse.".to_owned()));
        assert_eq!(compose(false, None), BASE.trim());
    }

    #[test]
    fn the_custom_prompt_follows_the_base_and_code_mode() {
        let composed = compose(true, Some("Be terse."));
        let base = composed.find("[Identity]").unwrap();
        let connectors = composed.find("[Connectors]").unwrap();
        let tools = composed
            .find(centaur_codemode_protocol::tools::INSTRUCTIONS)
            .unwrap();
        let custom = composed.find("Be terse.").unwrap();
        assert!(base < connectors && connectors < tools && tools < custom);
        assert!(composed.ends_with("---\n\nBe terse."));

        let without = compose(false, Some("Be terse."));
        assert!(!without.contains("[Connectors]"));
        assert!(!without.contains(centaur_codemode_protocol::tools::INSTRUCTIONS));
        assert!(without.ends_with("Be terse."));
    }
}
