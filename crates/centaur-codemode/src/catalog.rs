//! Connector discovery and compilation.
//!
//! A connector is a directory holding `connector.toml` and `index.ts`. Every
//! exported function in `index.ts` is a tool. Loading strips TypeScript to
//! JavaScript for QuickJS and emits declarations (without `tsc`) so cells and
//! search can show typed signatures.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use eyre::{Result, WrapErr, bail, eyre};
use oxc::{
    allocator::Allocator,
    ast::ast::{Declaration, Statement},
    codegen::{Codegen, CodegenOptions, CommentOptions},
    isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions},
    parser::Parser,
    semantic::SemanticBuilder,
    span::SourceType,
    transformer::{TransformOptions, Transformer},
};
use serde::{Deserialize, Serialize};

const MANIFEST: &str = "connector.toml";
const ENTRY: &str = "index.ts";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    description: String,
    // Parsed by the control plane (proxy config); the runtime only checks
    // they are well-formed arrays.
    #[serde(default)]
    #[expect(dead_code, reason = "validated here, consumed by the control plane")]
    secrets: Vec<toml::Value>,
    #[serde(default)]
    #[expect(dead_code, reason = "validated here, consumed by the control plane")]
    optional_secrets: Vec<toml::Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Function {
    pub name: String,
    pub doc: String,
    /// `name(args: {...}): Promise<...>` as cells call it (no `ctx`).
    pub signature: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Connector {
    pub name: String,
    pub description: String,
    #[serde(skip)]
    pub dir: PathBuf,
    /// QuickJS bytecode for the compiled ES module, built once per load and
    /// loaded into a fresh runtime for every call.
    #[serde(skip)]
    pub bytecode: Arc<[u8]>,
    pub functions: Vec<Function>,
    /// `.d.ts` text for the module, with the `ctx` parameter removed.
    pub declarations: String,
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub connectors: BTreeMap<String, Connector>,
}

impl Catalog {
    /// Load every connector under `roots`. Later roots shadow earlier ones by
    /// connector name, matching overlay order.
    pub fn load(roots: &[PathBuf]) -> Result<Self> {
        let mut connectors = BTreeMap::new();
        for root in roots {
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
                .wrap_err_with(|| format!("reading connector root {}", root.display()))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.join(MANIFEST).is_file())
                .collect();
            dirs.sort();
            for dir in dirs {
                let connector = load_connector(&dir)
                    .wrap_err_with(|| format!("loading connector {}", dir.display()))?;
                if let Some(previous) = connectors.insert(connector.name.clone(), connector) {
                    tracing::info!(
                        connector = %previous.name,
                        shadowed = %previous.dir.display(),
                        "connector shadowed by a later root"
                    );
                }
            }
        }
        Ok(Self { connectors })
    }

    /// Connectors visible for an allowlist; `None` means all.
    pub fn visible<'a>(
        &'a self,
        allowed: Option<&'a [String]>,
    ) -> impl Iterator<Item = &'a Connector> + 'a {
        self.connectors.values().filter(move |connector| {
            allowed.is_none_or(|allowed| allowed.contains(&connector.name))
        })
    }

    /// The brief an agent reads before writing a cell.
    pub fn brief(&self, allowed: Option<&[String]>) -> String {
        let mut out = String::new();
        for connector in self.visible(allowed) {
            out.push_str(&format!(
                "// ── tools.{} ── {}\n{}\n",
                connector.name,
                connector.description,
                connector.declarations.trim_end()
            ));
            out.push('\n');
        }
        out
    }
}

fn load_connector(dir: &Path) -> Result<Connector> {
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre!("connector directory has no UTF-8 name"))?
        .to_owned();
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("connector name {name:?} must be lowercase letters, digits and underscores");
    }
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(dir.join(MANIFEST)).wrap_err("reading connector.toml")?,
    )
    .wrap_err("parsing connector.toml")?;
    let source = std::fs::read_to_string(dir.join(ENTRY)).wrap_err("reading index.ts")?;
    let compiled = compile(&source, &dir.join(ENTRY))?;
    if compiled.functions.is_empty() {
        bail!("index.ts exports no functions");
    }
    let bytecode = bytecode(&name, compiled.js)?;
    Ok(Connector {
        name,
        description: manifest.description.trim().to_owned(),
        dir: dir.to_owned(),
        bytecode,
        functions: compiled.functions,
        declarations: compiled.declarations,
    })
}

/// Compile the module to QuickJS bytecode. The module name appears in stack
/// traces from connector code.
fn bytecode(name: &str, js: String) -> Result<Arc<[u8]>> {
    let runtime = rquickjs::Runtime::new()?;
    let context = rquickjs::Context::full(&runtime)?;
    context.with(|ctx| {
        let module = rquickjs::Module::declare(ctx.clone(), format!("connector:{name}"), js)
            .map_err(|error| eyre!("{}", describe_js_error(&ctx, error)))?;
        let bytes = module
            .write(rquickjs::module::WriteOptions::default())
            .map_err(|error| eyre!("{}", describe_js_error(&ctx, error)))?;
        Ok(Arc::from(bytes))
    })
}

fn describe_js_error(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> String {
    use rquickjs::CatchResultExt;
    match Err::<(), _>(error).catch(ctx) {
        Err(rquickjs::CaughtError::Exception(exception)) => exception
            .message()
            .unwrap_or_else(|| "invalid module".to_owned()),
        Err(other) => other.to_string(),
        Ok(()) => String::new(),
    }
}

struct Compiled {
    js: String,
    functions: Vec<Function>,
    declarations: String,
}

fn compile(source: &str, path: &Path) -> Result<Compiled> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        bail!("{}", render(parsed.diagnostics, source));
    }
    let mut program = parsed.program;

    let functions = exported_functions(&program, source)?;

    let mut dts = IsolatedDeclarations::new(
        &allocator,
        IsolatedDeclarationsOptions {
            strip_internal: true,
        },
    )
    .build(&program);
    if !dts.diagnostics.is_empty() {
        bail!(
            "exported functions and types need explicit annotations:\n{}",
            render(dts.diagnostics, source)
        );
    }
    // Cells call `tools.<connector>.<fn>(args)`; the runtime passes `ctx`.
    dts.program
        .body
        .retain(|statement| !matches!(statement, Statement::ImportDeclaration(_)));
    for statement in dts.program.body.iter_mut() {
        if let Statement::ExportDeclaration(export) = statement
            && let Declaration::FunctionDeclaration(function) = &mut export.declaration
        {
            function.params.items.truncate(1);
        }
    }
    let declarations = Codegen::new()
        .with_options(CodegenOptions {
            comments: CommentOptions {
                jsdoc: true,
                ..CommentOptions::disabled()
            },
            ..CodegenOptions::default()
        })
        .build(&dts.program)
        .code;

    let semantic = SemanticBuilder::new().build(&program);
    if !semantic.diagnostics.is_empty() {
        bail!("{}", render(semantic.diagnostics, source));
    }
    let scoping = semantic.semantic.into_scoping();
    let transformed = Transformer::new(&allocator, path, &TransformOptions::default())
        .build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        bail!("{}", render(transformed.diagnostics, source));
    }
    let js = Codegen::new()
        .with_options(CodegenOptions {
            comments: CommentOptions::disabled(),
            ..CodegenOptions::default()
        })
        .build(&program)
        .code;

    Ok(Compiled {
        js,
        functions,
        declarations,
    })
}

fn exported_functions(program: &oxc::ast::ast::Program<'_>, source: &str) -> Result<Vec<Function>> {
    let mut functions = Vec::new();
    for statement in &program.body {
        match statement {
            Statement::ExportDeclaration(export) => {
                let Declaration::FunctionDeclaration(function) = &export.declaration else {
                    continue;
                };
                let name = function
                    .id
                    .as_ref()
                    .map(|id| id.name.to_string())
                    .ok_or_else(|| eyre!("exported function without a name"))?;
                if !function.r#async {
                    bail!("exported function {name} must be async");
                }
                if function.params.items.len() > 2 || function.params.rest.is_some() {
                    bail!("exported function {name} must take (args, ctx)");
                }
                let args = function
                    .params
                    .items
                    .first()
                    .map(|param| param.span.source_text(source).to_owned())
                    .unwrap_or_default();
                let returns = function
                    .return_type
                    .as_ref()
                    .map(|annotation| annotation.span.source_text(source))
                    .unwrap_or(": Promise<unknown>");
                let doc = program
                    .comments
                    .iter()
                    .rfind(|comment| comment.is_jsdoc() && comment.attached_to == export.span.start)
                    .map(|comment| clean_jsdoc(comment.content_span().source_text(source)))
                    .unwrap_or_default();
                functions.push(Function {
                    signature: format!("{name}({args}){returns}"),
                    name,
                    doc,
                });
            }
            Statement::ExportDefaultDeclaration(_) => {
                bail!("connectors use named exports, not export default");
            }
            _ => {}
        }
    }
    Ok(functions)
}

fn clean_jsdoc(raw: &str) -> String {
    raw.trim_start_matches('*')
        .lines()
        .map(|line| line.trim().trim_start_matches('*').trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render(
    diagnostics: impl IntoIterator<Item = oxc::diagnostics::OxcDiagnostic>,
    source: &str,
) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| format!("{:?}", diagnostic.with_source_code(source.to_owned())))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
import type { Ctx } from "../centaur";

export interface Thing { id: string; size?: number }

/** Find things by name.
 *  Returns at most `limit`. */
export async function find_things(args: { query: string; limit?: number }, ctx: Ctx): Promise<{ things: Thing[] }> {
  const res = await ctx.fetch("https://example.com/?q=" + encodeURIComponent(args.query));
  return { things: (await res.json()) as Thing[] };
}

function helper(x: number): number { return x + 1; }
"#;

    #[test]
    fn compiles_exports_docs_and_declarations() {
        let compiled = compile(SOURCE, Path::new("index.ts")).unwrap();
        assert_eq!(compiled.functions.len(), 1);
        let function = &compiled.functions[0];
        assert_eq!(function.name, "find_things");
        assert_eq!(
            function.doc,
            "Find things by name. Returns at most `limit`."
        );
        assert_eq!(
            function.signature,
            "find_things(args: { query: string; limit?: number }): Promise<{ things: Thing[] }>"
        );
        assert!(!compiled.js.contains("interface"), "{}", compiled.js);
        assert!(
            compiled
                .js
                .contains("export async function find_things(args, ctx)")
        );
        assert!(compiled.declarations.contains("export interface Thing"));
        assert!(
            !compiled.declarations.contains("ctx"),
            "{}",
            compiled.declarations
        );
        assert!(!compiled.declarations.contains("import"));
        assert!(compiled.declarations.contains("Find things by name."));
    }

    #[test]
    fn repository_connectors_compile() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../connectors");
        let catalog = Catalog::load(&[root]).unwrap();
        for name in ["crunchbase", "slack"] {
            let connector = &catalog.connectors[name];
            assert!(!connector.functions.is_empty(), "{name} exports tools");
            for function in &connector.functions {
                assert!(
                    !function.doc.is_empty(),
                    "{name}.{} needs a JSDoc comment",
                    function.name
                );
            }
        }
    }

    #[test]
    fn rejects_unannotated_exports() {
        let error = compile(
            "export async function f(args: { a: string }) { return 1; }",
            Path::new("index.ts"),
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("explicit annotations"),
            "{error}"
        );
    }

    #[test]
    fn rejects_sync_exports() {
        let error = compile(
            "export function f(args: { a: string }): number { return 1; }",
            Path::new("index.ts"),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("must be async"), "{error}");
    }
}
