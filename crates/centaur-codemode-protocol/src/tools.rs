//! The three Code Mode tools an agent sees, and how their results render.
//!
//! api-rs serves them over MCP and the brain registers them as native
//! nanocode tools; both use these definitions and renderers so the model sees
//! the same contract either way.

use serde_json::{Value, json};

pub const SEARCH: &str = "centaur_catalog_search";
pub const LOAD: &str = "centaur_catalog_load";
pub const EXECUTE: &str = "centaur_execute";

/// Guidance for the model, given as MCP server instructions or as developer
/// instructions in the brain.
pub const INSTRUCTIONS: &str = concat!(
    "Centaur Code Mode exposes a large, dynamic connector catalog through a few tools. ",
    "Whenever a task involves an external service, private or current data, an external ",
    "action, or a service URL, first call `centaur_catalog_search` with the relevant ",
    "service names, URLs, domains and action; results include each function's ",
    "signature. Load a connector's full TypeScript declarations with ",
    "`centaur_catalog_load` when the one-line signature is not enough. Then call ",
    "connectors by writing JavaScript for `centaur_execute`: `code` is the body of an ",
    "async function, call connectors as ",
    "`await tools.<connector>.<function>(args)`, run independent calls together with ",
    "`Promise.all` or `Promise.allSettled` in one cell instead of several tool calls, ",
    "and `return` the JSON-serializable value you want back. `console.log` output is ",
    "returned with the result. A cell has no `fetch`, no module imports, no file access ",
    "and no state between calls, and is limited to 120 s, 1000 calls and 8 concurrent ",
    "calls."
);

/// One tool: its name, description and JSON Schema for its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

impl ToolSpec {
    /// As an MCP `tools/list` entry.
    #[must_use]
    pub fn mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        })
    }
}

/// The search, load and execute tools, in that order.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: SEARCH,
            description: concat!(
                "Search Centaur's live connector catalog. The catalog is large and dynamic, so ",
                "search whenever a task involves an external service, private or current data, an ",
                "external action, or a service URL; include service names, URLs, domains and the ",
                "requested action as terms. Results include each function's signature. Load a ",
                "connector's full TypeScript declarations with `centaur_catalog_load` when needed."
            ),
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "additionalProperties": false,
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "description": "Maximum number of results (default 8).",
                    },
                },
            }),
        },
        ToolSpec {
            name: LOAD,
            description: concat!(
                "Load a connector's full TypeScript declarations and JSDoc, for a connector found ",
                "with `centaur_catalog_search`. Use it when the one-line signature from search is ",
                "not enough to call the connector correctly."
            ),
            input_schema: json!({
                "type": "object",
                "required": ["tool"],
                "additionalProperties": false,
                "properties": {
                    "tool": {"type": "string", "minLength": 1},
                },
            }),
        },
        ToolSpec {
            name: EXECUTE,
            description: concat!(
                "Run one JavaScript cell against Centaur's connectors. `code` is the body of ",
                "an async function: call connectors as `await tools.<connector>.<function>(args)`, ",
                "run independent calls together with `Promise.all` or `Promise.allSettled` in the ",
                "same cell instead of several tool calls, and `return` a JSON-serializable value, ",
                "which becomes the result. `console.log` output is returned too. There is no ",
                "`fetch`, no module imports, no file access and no state between cells. A cell is ",
                "limited to 120 s, 1000 calls and 8 concurrent calls."
            ),
            input_schema: json!({
                "type": "object",
                "required": ["code"],
                "additionalProperties": false,
                "properties": {
                    "code": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Async function body; `return` value is the cell result.",
                    },
                },
            }),
        },
    ]
}

/// A rendered tool result: the text the model reads, whether it is an error,
/// and the structured value MCP clients may use.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub text: String,
    pub is_error: bool,
    pub structured: Option<Value>,
}

impl Rendered {
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            structured: None,
        }
    }
}

/// A search response (`{results, catalog_revision}`), shown as pretty JSON.
#[must_use]
pub fn render_search(body: Value) -> Rendered {
    Rendered {
        text: serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
        is_error: false,
        structured: Some(body),
    }
}

/// A load response (`{tool, description, brief}`): the description, then the
/// declarations.
#[must_use]
pub fn render_load(body: &Value) -> Rendered {
    let text = ["description", "brief"]
        .iter()
        .filter_map(|field| body.get(field).and_then(Value::as_str).map(str::trim))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Rendered {
        text,
        is_error: false,
        structured: None,
    }
}

/// The error for a connector a load could not find.
#[must_use]
pub fn render_unknown_tool(tool: &str) -> Rendered {
    Rendered::error(format!(
        "Unknown or unavailable tool {tool}. Use centaur_catalog_search to discover available tools."
    ))
}

/// A cell response (`{ok, result?, error?, logs}`). A failed cell is an error
/// result carrying its message, the failing tool, its stack and the logs.
pub fn render_exec(body: Value) -> Result<Rendered, serde_json::Error> {
    let response: ExecOutcome = serde_json::from_value(body)?;
    if response.ok {
        let mut text = match response.result.as_ref() {
            Some(result) => serde_json::to_string_pretty(result)?,
            None => "null".to_owned(),
        };
        append_logs(&mut text, &response.logs);
        return Ok(Rendered {
            text,
            is_error: false,
            structured: Some(json!({
                "result": response.result,
                "logs": response.logs,
            })),
        });
    }
    let failure = response.error.unwrap_or_default();
    let mut text = if failure.message.is_empty() {
        "Code Mode cell failed".to_owned()
    } else {
        failure.message
    };
    for extra in [
        failure.tool.map(|tool| format!("tool: {tool}")),
        failure.stack,
    ]
    .into_iter()
    .flatten()
    {
        text.push_str(&format!("\n\n{extra}"));
    }
    append_logs(&mut text, &response.logs);
    Ok(Rendered::error(text))
}

fn append_logs(text: &mut String, logs: &[String]) {
    if logs.is_empty() {
        return;
    }
    text.push_str("\n\nlogs:\n");
    text.push_str(&logs.join("\n"));
}

#[derive(serde::Deserialize)]
struct ExecOutcome {
    ok: bool,
    result: Option<Value>,
    error: Option<ExecFailure>,
    #[serde(default)]
    logs: Vec<String>,
}

#[derive(Default, serde::Deserialize)]
struct ExecFailure {
    #[serde(default)]
    message: String,
    #[serde(default)]
    stack: Option<String>,
    #[serde(default)]
    tool: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_search_load_execute() {
        let names: Vec<_> = specs().iter().map(|spec| spec.name).collect();
        assert_eq!(names, [SEARCH, LOAD, EXECUTE]);
        assert_eq!(specs()[2].mcp()["inputSchema"]["required"], json!(["code"]));
    }

    #[test]
    fn renders_cells() {
        let ok = render_exec(json!({"ok": true, "result": {"a": 1}, "logs": ["hi"]})).unwrap();
        assert!(!ok.is_error);
        assert_eq!(ok.text, "{\n  \"a\": 1\n}\n\nlogs:\nhi");

        let failed = render_exec(json!({
            "ok": false,
            "error": {"message": "boom", "tool": "slack.channels"},
            "logs": [],
        }))
        .unwrap();
        assert!(failed.is_error);
        assert_eq!(failed.text, "boom\n\ntool: slack.channels");
    }

    #[test]
    fn renders_loads() {
        let rendered = render_load(&json!({"description": "Slack", "brief": "declare ..."}));
        assert_eq!(rendered.text, "Slack\n\ndeclare ...");
    }
}
