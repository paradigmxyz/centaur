//! Connector discovery and compilation.
//!
//! A connector is a directory holding `connector.toml` and `index.ts`. Every
//! exported function in `index.ts` is a tool. Helpers shared by connectors
//! live in `_lib/*.ts` next to them and are imported as `../_lib/<name>`. Loading strips TypeScript to
//! JavaScript for QuickJS and emits declarations (without `tsc`) so the brief
//! can show typed signatures.
//!
//! A function whose JSDoc carries `@requires files` only works with the files
//! capability. A catalog loaded without it leaves such functions out of
//! search, the brief and `invoke`, and drops a connector left with none.

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
use sha2::{Digest, Sha256};

use crate::search::{self, Document, Hit, Index};

const MANIFEST: &str = "connector.toml";
const ENTRY: &str = "index.ts";
const LIB_DIR: &str = "_lib";
/// Types only; there is no runtime module behind it.
const TYPES_MODULE: &str = "../centaur";
/// Hex characters of the catalog revision that are kept.
const REVISION_CHARS: usize = 16;
/// The JSDoc tag that marks a function as needing the files capability.
const REQUIRES_FILES_TAG: &str = "@requires files";
/// `_lib/files.ts` helpers that throw without the files capability.
const FILES_HELPERS: [&str; 3] = ["requireFiles", "downloadTo", "saveBytes"];

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
    /// Tagged `@requires files`: callable only with the files capability.
    #[serde(skip)]
    pub requires_files: bool,
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
    /// Shared helper modules by name (`_lib/<name>.ts`), as bytecode.
    pub libs: Arc<BTreeMap<String, Arc<[u8]>>>,
    /// Content hash of the connector roots this catalog was loaded from.
    /// `/healthz` and `/search` report it; a hot reload compares it.
    pub revision: String,
    /// Whether this catalog keeps `@requires files` functions.
    pub files: bool,
    /// One BM25 document per connector function, built with the catalog.
    search: Index,
}

impl Catalog {
    /// Load every connector under `roots`. Later roots shadow earlier ones by
    /// connector name, matching overlay order. `files` is whether the runner
    /// has the files capability; without it, `@requires files` functions are
    /// left out.
    pub fn load(roots: &[PathBuf], files: bool) -> Result<Self> {
        // Hash before reading anything. A file that changes during the load
        // then leaves the recorded revision behind the tree, so the next
        // reload check sees a difference and loads again; hashing afterwards
        // could label stale bytecode with the newest revision and never
        // reload it.
        let revision = revision(roots)?;
        // Phase 1: strip types and collect imports. Later roots shadow
        // earlier ones, for connectors and shared modules alike.
        let mut connectors: BTreeMap<String, (Connector, String)> = BTreeMap::new();
        let mut libs: BTreeMap<String, String> = BTreeMap::new();
        let mut imports: Vec<(String, Vec<Import>)> = Vec::new();
        for root in roots {
            // Overlay roots a principal cannot see are not mounted, and an
            // overlay without connectors has no root: expected, not a problem.
            if !root.is_dir() {
                tracing::info!(root = %root.display(), "connector root is missing; skipping");
                continue;
            }
            let lib_dir = root.join(LIB_DIR);
            if lib_dir.is_dir() {
                let mut files: Vec<PathBuf> = std::fs::read_dir(&lib_dir)?
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
                    .collect();
                files.sort();
                for file in files {
                    let name = file
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .ok_or_else(|| eyre!("{} has no UTF-8 name", file.display()))?
                        .to_owned();
                    let source = std::fs::read_to_string(&file)?;
                    let compiled = compile_lib(&source, &file)
                        .wrap_err_with(|| format!("loading {}", file.display()))?;
                    imports.push((file.display().to_string(), compiled.imports));
                    libs.insert(name, compiled.js);
                }
            }
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
                .wrap_err_with(|| format!("reading connector root {}", root.display()))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.join(MANIFEST).is_file())
                .collect();
            dirs.sort();
            for dir in dirs {
                let (connector, connector_imports, js) = load_connector(&dir, files)
                    .wrap_err_with(|| format!("loading connector {}", dir.display()))?;
                imports.push((dir.display().to_string(), connector_imports));
                if let Some((previous, _)) =
                    connectors.insert(connector.name.clone(), (connector, js))
                {
                    tracing::info!(
                        connector = %previous.name,
                        shadowed = %previous.dir.display(),
                        "connector shadowed by a later root"
                    );
                }
            }
        }
        // After shadowing, so an overlay that only offers file functions
        // still hides the connector it shadows.
        connectors.retain(|name, (connector, _)| {
            let keep = !connector.functions.is_empty();
            if !keep {
                tracing::info!(connector = %name, "every function needs files; leaving the connector out");
            }
            keep
        });
        for (module, imports) in &imports {
            for import in imports {
                check_import(import, &libs).wrap_err_with(|| format!("in {module}"))?;
            }
        }
        // Phase 2: bytecode. QuickJS resolves imports even when only
        // compiling, so every module compiles with the shared modules' JS
        // available.
        let lib_bytecode = libs
            .iter()
            .map(|(name, js)| Ok((name.clone(), bytecode(&format!("lib:{name}"), js, &libs)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let connectors = connectors
            .into_values()
            .map(|(mut connector, js)| {
                connector.bytecode = bytecode(&format!("connector:{}", connector.name), &js, &libs)
                    .wrap_err_with(|| format!("compiling connector {}", connector.name))?;
                Ok((connector.name.clone(), connector))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let search = search::Index::new(
            connectors
                .values()
                .flat_map(|connector| {
                    connector.functions.iter().map(|function| Document {
                        connector: connector.name.clone(),
                        description: connector.description.clone(),
                        function: function.name.clone(),
                        signature: function.signature.clone(),
                        doc: function.doc.clone(),
                    })
                })
                .collect(),
        );
        Ok(Self {
            connectors,
            libs: Arc::new(lib_bytecode),
            revision,
            files,
            search,
        })
    }

    /// Connectors visible under a policy; no policy means all of them.
    pub fn visible<'a>(
        &'a self,
        policy: Option<&'a Policy>,
    ) -> impl Iterator<Item = &'a Connector> + 'a {
        self.connectors
            .values()
            .filter(move |connector| policy.is_none_or(|policy| policy.allows(&connector.name)))
    }

    /// The connector a request names exactly, when the policy makes it
    /// visible.
    pub fn find<'a>(
        &'a self,
        requested: &str,
        policy: Option<&'a Policy>,
    ) -> Option<&'a Connector> {
        self.visible(policy)
            .find(|connector| connector.name == requested)
    }

    /// The brief an agent reads before writing a cell.
    pub fn brief(&self, policy: Option<&Policy>) -> String {
        let mut out = String::new();
        for connector in self.visible(policy) {
            out.push_str(&connector_brief(connector));
            out.push('\n');
        }
        out
    }

    /// BM25 search over the catalog, keeping only connectors the policy makes
    /// visible, then truncating to `limit`.
    pub fn search(&self, query: &str, limit: usize, policy: Option<&Policy>) -> Vec<Hit> {
        self.search
            .ranked(query)
            .into_iter()
            .filter_map(|(index, _)| self.search.documents().get(index))
            .filter(|document| policy.is_none_or(|policy| policy.allows(&document.connector)))
            .take(limit)
            .map(Document::hit)
            .collect()
    }
}

/// One connector's TypeScript declarations, with the header the brief uses.
pub fn connector_brief(connector: &Connector) -> String {
    format!(
        "// ── tools.{} ── {}\n{}\n",
        connector.name,
        connector.description,
        connector.declarations.trim_end()
    )
}

/// Content revision of the connector roots: a hash over every file a load
/// reads (manifests, TypeScript and the shared declarations), in a
/// deterministic order. Missing roots contribute nothing, so a root that is
/// not mounted does not change what a load would do.
pub fn revision(roots: &[PathBuf]) -> Result<String> {
    let mut hash = Sha256::new();
    for (ordinal, root) in roots.iter().enumerate() {
        if !root.is_dir() {
            continue;
        }
        // Overlay order decides shadowing, so it is part of the content.
        hash.update(ordinal.to_be_bytes());
        for file in root_files(root)? {
            let relative = file.strip_prefix(root).unwrap_or(&file);
            hash.update(relative.to_string_lossy().as_bytes());
            hash.update([0]);
            hash.update(
                std::fs::read(&file).wrap_err_with(|| format!("reading {}", file.display()))?,
            );
            hash.update([0]);
        }
    }
    Ok(hex::encode(hash.finalize())[..REVISION_CHARS].to_owned())
}

/// The files under one root that a load depends on, sorted by path.
fn root_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .wrap_err_with(|| format!("reading connector root {}", root.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    let mut files = Vec::new();
    for entry in entries {
        if entry.is_dir() {
            // A connector directory, or the shared `_lib` modules.
            let named = entry.file_name().and_then(|name| name.to_str());
            if named != Some(LIB_DIR) && !entry.join(MANIFEST).is_file() {
                continue;
            }
            let mut nested: Vec<PathBuf> = std::fs::read_dir(&entry)
                .wrap_err_with(|| format!("reading {}", entry.display()))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_file() && (path.ends_with(MANIFEST) || is_typescript(path)))
                .collect();
            nested.sort();
            files.extend(nested);
        } else if is_typescript(&entry) {
            // `centaur.d.ts` and any other root-level declarations.
            files.push(entry);
        }
    }
    Ok(files)
}

fn is_typescript(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "ts")
}

/// The shared module an import names, if it names one: `../_lib/<name>` from
/// a connector, `./<name>` from another shared module.
pub fn lib_name(specifier: &str) -> Option<&str> {
    let name = specifier
        .strip_prefix("../_lib/")
        .or_else(|| specifier.strip_prefix("./"))?;
    let name = name
        .strip_suffix(".ts")
        .or_else(|| name.strip_suffix(".js"))
        .unwrap_or(name);
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

#[derive(Debug)]
struct Import {
    specifier: String,
    type_only: bool,
}

fn check_import(import: &Import, libs: &BTreeMap<String, String>) -> Result<()> {
    if import.specifier == TYPES_MODULE {
        if !import.type_only {
            bail!("`{TYPES_MODULE}` holds types only; use `import type`");
        }
        return Ok(());
    }
    match lib_name(&import.specifier) {
        Some(name) if libs.contains_key(name) => Ok(()),
        Some(name) => bail!(
            "imports `{}`, but there is no _lib/{name}.ts",
            import.specifier
        ),
        None => bail!(
            "imports `{}`; connectors may import only `{TYPES_MODULE}` (types) and `../_lib/<name>`",
            import.specifier
        ),
    }
}

fn imports(program: &oxc::ast::ast::Program<'_>) -> Vec<Import> {
    program
        .body
        .iter()
        .filter_map(|statement| match statement {
            Statement::ImportDeclaration(import) => Some(Import {
                specifier: import.source.value.as_str().unwrap_or_default().to_owned(),
                type_only: import.import_kind.is_type(),
            }),
            _ => None,
        })
        .collect()
}

fn load_connector(dir: &Path, files: bool) -> Result<(Connector, Vec<Import>, String)> {
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
    let compiled = compile(&source, &dir.join(ENTRY), files)?;
    if compiled.exported == 0 {
        bail!("index.ts exports no functions");
    }
    // A missing tag only leaves a function visible where it cannot work, so
    // warn instead of refusing a connector an overlay may ship.
    for function in &compiled.untagged_files {
        tracing::warn!(
            "{name}.{function} always needs the files capability; tag its JSDoc `{REQUIRES_FILES_TAG}`"
        );
    }
    let connector = Connector {
        name,
        description: manifest.description.trim().to_owned(),
        dir: dir.to_owned(),
        bytecode: Arc::from([]),
        functions: compiled.functions,
        declarations: compiled.declarations,
    };
    Ok((connector, compiled.imports, compiled.js))
}

/// Compile a module to QuickJS bytecode, in its own runtime so each shared
/// module is declared once. The module name appears in stack traces and is
/// what imports resolve against.
fn bytecode(module_name: &str, js: &str, libs: &BTreeMap<String, String>) -> Result<Arc<[u8]>> {
    let runtime = rquickjs::Runtime::new()?;
    runtime.set_loader(SourceResolver(libs.clone()), SourceLoader(libs.clone()));
    let context = rquickjs::Context::full(&runtime)?;
    context.with(|ctx| {
        let module = rquickjs::Module::declare(ctx.clone(), module_name, js)
            .map_err(|error| eyre!("{}", describe_js_error(&ctx, error)))?;
        let bytes = module
            .write(rquickjs::module::WriteOptions::default())
            .map_err(|error| eyre!("{}", describe_js_error(&ctx, error)))?;
        Ok(Arc::from(bytes))
    })
}

/// Compile-time resolution of shared modules, from their stripped JS.
struct SourceResolver(BTreeMap<String, String>);

impl rquickjs::loader::Resolver for SourceResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &rquickjs::Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        match lib_name(name) {
            Some(lib) if self.0.contains_key(lib) => Ok(format!("lib:{lib}")),
            _ => Err(rquickjs::Error::new_resolving(base, name)),
        }
    }
}

struct SourceLoader(BTreeMap<String, String>);

impl rquickjs::loader::Loader for SourceLoader {
    fn load<'js>(
        &mut self,
        ctx: &rquickjs::Ctx<'js>,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<rquickjs::Module<'js, rquickjs::module::Declared>> {
        let js = name
            .strip_prefix("lib:")
            .and_then(|lib| self.0.get(lib))
            .ok_or_else(|| rquickjs::Error::new_loading(name))?;
        rquickjs::Module::declare(ctx.clone(), name, js.as_str())
    }
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
    /// The functions this catalog offers: all exports, less `@requires
    /// files` ones when there is no files capability.
    functions: Vec<Function>,
    /// How many functions the module exports, offered or not.
    exported: usize,
    /// Exports that always call a files helper but lack the tag.
    untagged_files: Vec<String>,
    declarations: String,
    imports: Vec<Import>,
}

struct CompiledLib {
    js: String,
    imports: Vec<Import>,
}

/// A shared module: TypeScript stripped, no export rules or declarations.
fn compile_lib(source: &str, path: &Path) -> Result<CompiledLib> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    if !parsed.diagnostics.is_empty() {
        bail!("{}", render(parsed.diagnostics, source));
    }
    let mut program = parsed.program;
    let imports = imports(&program);
    let js = strip_types(&allocator, &mut program, source, path)?;
    Ok(CompiledLib { js, imports })
}

fn strip_types<'a>(
    allocator: &'a Allocator,
    program: &mut oxc::ast::ast::Program<'a>,
    source: &str,
    path: &Path,
) -> Result<String> {
    let semantic = SemanticBuilder::new().build(program);
    if !semantic.diagnostics.is_empty() {
        bail!("{}", render(semantic.diagnostics, source));
    }
    let scoping = semantic.semantic.into_scoping();
    let transformed = Transformer::new(allocator, path, &TransformOptions::default())
        .build_with_scoping(scoping, program);
    if !transformed.diagnostics.is_empty() {
        bail!("{}", render(transformed.diagnostics, source));
    }
    Ok(Codegen::new()
        .with_options(CodegenOptions {
            comments: CommentOptions::disabled(),
            ..CodegenOptions::default()
        })
        .build(program)
        .code)
}

fn compile(source: &str, path: &Path, files: bool) -> Result<Compiled> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        bail!("{}", render(parsed.diagnostics, source));
    }
    let mut program = parsed.program;

    let mut functions = exported_functions(&program, source)?;
    let exported = functions.len();
    let untagged_files = untagged_files_functions(&program, &functions);
    let imports = imports(&program);
    let hidden: Vec<String> = if files {
        Vec::new()
    } else {
        functions
            .iter()
            .filter(|function| function.requires_files)
            .map(|function| function.name.clone())
            .collect()
    };
    functions.retain(|function| !hidden.contains(&function.name));

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
    dts.program.body.retain(|statement| {
        !matches!(statement, Statement::ExportDeclaration(export)
            if matches!(&export.declaration, Declaration::FunctionDeclaration(function)
                if function.id.as_ref().is_some_and(|id| hidden.iter().any(|name| id.name == name.as_str()))))
    });
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

    let js = strip_types(&allocator, &mut program, source, path)?;

    Ok(Compiled {
        js,
        functions,
        exported,
        untagged_files,
        declarations,
        imports,
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
                let raw_doc = program
                    .comments
                    .iter()
                    .rfind(|comment| comment.is_jsdoc() && comment.attached_to == export.span.start)
                    .map(|comment| comment.content_span().source_text(source))
                    .unwrap_or_default();
                functions.push(Function {
                    signature: format!("{name}({args}){returns}"),
                    name,
                    doc: clean_jsdoc(raw_doc),
                    requires_files: requires_files(raw_doc),
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

fn jsdoc_lines(raw: &str) -> impl Iterator<Item = &str> {
    raw.trim_start_matches('*')
        .lines()
        .map(|line| line.trim().trim_start_matches('*').trim())
}

/// The description: the JSDoc without blank lines or the files tag.
fn clean_jsdoc(raw: &str) -> String {
    jsdoc_lines(raw)
        .filter(|line| !line.is_empty() && !is_requires_files(line))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_requires_files(line: &str) -> bool {
    line.strip_prefix(REQUIRES_FILES_TAG)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn requires_files(raw_doc: &str) -> bool {
    jsdoc_lines(raw_doc).any(is_requires_files)
}

/// Exported functions whose body always calls a `_lib/files.ts` helper that
/// throws without the files capability, yet lack the tag. Only top-level
/// statements count, and not inside a branch or nested function, so a
/// function that touches files only for some arguments is not flagged. A
/// helper reached through a local function is not seen either: the tag is
/// the source of truth, this only catches the common omission.
fn untagged_files_functions(
    program: &oxc::ast::ast::Program<'_>,
    functions: &[Function],
) -> Vec<String> {
    use oxc::ast_visit::Visit;

    #[derive(Default)]
    struct FindsHelper(bool);

    impl<'a> Visit<'a> for FindsHelper {
        fn visit_call_expression(&mut self, call: &oxc::ast::ast::CallExpression<'a>) {
            if let oxc::ast::ast::Expression::Identifier(callee) = &call.callee
                && FILES_HELPERS.contains(&callee.name.as_str())
            {
                self.0 = true;
            }
            oxc::ast_visit::walk::walk_call_expression(self, call);
        }
        // Branches and nested functions run only sometimes.
        fn visit_conditional_expression(&mut self, _: &oxc::ast::ast::ConditionalExpression<'a>) {}
        fn visit_logical_expression(&mut self, _: &oxc::ast::ast::LogicalExpression<'a>) {}
        fn visit_function(
            &mut self,
            _: &oxc::ast::ast::Function<'a>,
            _: oxc::syntax::scope::ScopeFlags,
        ) {
        }
        fn visit_arrow_function_expression(
            &mut self,
            _: &oxc::ast::ast::ArrowFunctionExpression<'a>,
        ) {
        }
    }

    let mut untagged = Vec::new();
    for statement in &program.body {
        let Statement::ExportDeclaration(export) = statement else {
            continue;
        };
        let Declaration::FunctionDeclaration(function) = &export.declaration else {
            continue;
        };
        let (Some(id), Some(body)) = (&function.id, &function.body) else {
            continue;
        };
        let tagged = functions
            .iter()
            .any(|f| f.name == id.name.as_str() && f.requires_files);
        if tagged {
            continue;
        }
        let mut finds = FindsHelper::default();
        for statement in &body.statements {
            match statement {
                Statement::ExpressionStatement(_)
                | Statement::VariableDeclaration(_)
                | Statement::ReturnStatement(_) => finds.visit_statement(statement),
                _ => {}
            }
        }
        if finds.0 {
            untagged.push(id.name.to_string());
        }
    }
    untagged
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

/// Which connectors a request may see: the deployment-wide tool filter api-rs
/// sends with each request. An entry matches a connector only when it equals
/// the connector's name exactly, so `google-docs` does not match `google_docs`.
/// No policy, or neither list set, means every connector. Nothing here
/// authorizes a credential: iron-proxy only injects what a principal holds.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Policy {
    #[serde(default)]
    pub allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub blocklist: Option<Vec<String>>,
}

impl Policy {
    /// An allowlist only, from the CLI's `--tools` or the request's `tools`.
    pub fn from_allowlist(names: Vec<String>) -> Self {
        Self {
            allowlist: Some(names),
            blocklist: None,
        }
    }

    /// Not blocklisted, and allowlisted when there is an allowlist.
    pub fn allows(&self, name: &str) -> bool {
        let listed = |entries: &Option<Vec<String>>| {
            entries
                .as_deref()
                .is_some_and(|entries| entries.iter().any(|entry| entry == name))
        };
        !listed(&self.blocklist) && (self.allowlist.is_none() || listed(&self.allowlist))
    }
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
        let compiled = compile(SOURCE, Path::new("index.ts"), true).unwrap();
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
        let catalog = Catalog::load(&[root], true).unwrap();
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
    fn repository_file_functions_are_tagged() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../connectors");
        let mut untagged = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap() {
            let dir = entry.unwrap().path();
            let Ok(source) = std::fs::read_to_string(dir.join(ENTRY)) else {
                continue;
            };
            let compiled = compile(&source, &dir.join(ENTRY), true).unwrap();
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            untagged.extend(
                compiled
                    .untagged_files
                    .iter()
                    .map(|f| format!("{name}.{f}")),
            );
        }
        assert!(
            untagged.is_empty(),
            "tag these `{REQUIRES_FILES_TAG}`: {untagged:?}"
        );

        let without = Catalog::load(std::slice::from_ref(&root), false).unwrap();
        let with = Catalog::load(&[root], true).unwrap();
        assert!(
            with.connectors["slack"]
                .functions
                .iter()
                .any(|f| f.name == "download_file")
        );
        assert!(
            !without.connectors["slack"]
                .functions
                .iter()
                .any(|f| f.name == "download_file")
        );
        assert!(
            without.connectors["slack"]
                .functions
                .iter()
                .any(|f| f.name == "channels")
        );
    }

    const FILES: &str = r#"
import type { Ctx } from "../centaur";
import { requireFiles } from "../_lib/files";

/**
 * Save something.
 *
 * @requires files
 */
export async function save(args: { path: string }, ctx: Ctx): Promise<{ path: string }> {
  requireFiles(ctx, "demo.save");
  return { path: args.path };
}

/** List things. */
export async function list(args: {}, ctx: Ctx): Promise<{ n: number }> { return { n: 1 }; }

/** Optionally save; only some arguments need files. */
export async function maybe(args: { path?: string }, ctx: Ctx): Promise<{}> {
  if (args.path) requireFiles(ctx, "demo.maybe");
  return {};
}

/** Forgot the tag. */
export async function untagged(args: {}, ctx: Ctx): Promise<{}> {
  const files = requireFiles(ctx, "demo.untagged");
  return {};
}
"#;

    const FILES_LIB: (&str, &str) = (
        "files",
        "export function requireFiles(ctx: any, what: string): any { return ctx.files; }",
    );

    #[test]
    fn file_functions_are_hidden_without_the_files_capability() {
        let compiled = compile(FILES, Path::new("index.ts"), true).unwrap();
        assert_eq!(compiled.untagged_files, ["untagged"]);
        let save = &compiled.functions[0];
        assert!(save.requires_files);
        assert_eq!(save.doc, "Save something.");
        assert!(compiled.declarations.contains("function save"));

        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(connector.join(MANIFEST), "description = \"Demo\"\n").unwrap();
        std::fs::write(connector.join(ENTRY), FILES).unwrap();
        std::fs::create_dir_all(dir.path().join(LIB_DIR)).unwrap();
        std::fs::write(dir.path().join(LIB_DIR).join("files.ts"), FILES_LIB.1).unwrap();
        let roots = [dir.path().to_owned()];

        let with = Catalog::load(&roots, true).unwrap();
        assert_eq!(with.connectors["demo"].functions.len(), 4);
        assert_eq!(with.search("save something", 8, None)[0].function, "save");

        let without = Catalog::load(&roots, false).unwrap();
        let demo = &without.connectors["demo"];
        let names: Vec<&str> = demo.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["list", "maybe", "untagged"]);
        assert!(
            !demo.declarations.contains("function save"),
            "{}",
            demo.declarations
        );
        assert!(demo.declarations.contains("function list"));
        assert!(
            without
                .search("save something", 8, None)
                .iter()
                .all(|hit| hit.function != "save")
        );

        // A connector with only file functions disappears.
        let only = "import type { Ctx } from \"../centaur\";\n/**\n * Save.\n * @requires files\n */\n\
                    export async function save(args: {}, ctx: Ctx): Promise<{}> { return {}; }\n";
        std::fs::write(connector.join(ENTRY), only).unwrap();
        assert!(Catalog::load(&roots, false).unwrap().connectors.is_empty());
        assert_eq!(Catalog::load(&roots, true).unwrap().connectors.len(), 1);
    }

    fn load_one(index: &str, libs: &[(&str, &str)]) -> Result<Catalog> {
        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(connector.join(MANIFEST), "description = \"Demo\"\n").unwrap();
        std::fs::write(connector.join(ENTRY), index).unwrap();
        std::fs::create_dir_all(dir.path().join(LIB_DIR)).unwrap();
        for (name, source) in libs {
            std::fs::write(dir.path().join(LIB_DIR).join(format!("{name}.ts")), source).unwrap();
        }
        Catalog::load(&[dir.path().to_owned()], true)
    }

    const USES_LIB: &str = r#"
import type { Ctx } from "../centaur";
import { twice } from "../_lib/math";
/** Doubles. */
export async function f(args: { n: number }, ctx: Ctx): Promise<{ n: number }> { return { n: twice(args.n) }; }
"#;

    #[test]
    fn shared_modules_compile_and_may_import_each_other() {
        let catalog = load_one(
            USES_LIB,
            &[
                ("math", "import { one } from \"./one\";\nexport const twice = (n: number): number => n * 2 * one;"),
                ("one", "export const one = 1;"),
            ],
        )
        .unwrap();
        assert_eq!(catalog.libs.len(), 2);
        assert!(!catalog.connectors["demo"].declarations.contains("_lib"));
    }

    #[test]
    fn rejects_imports_other_than_types_and_shared_modules() {
        let missing = load_one(USES_LIB, &[]).unwrap_err();
        assert!(
            format!("{missing:#}").contains("no _lib/math.ts"),
            "{missing:#}"
        );

        let node = USES_LIB.replace("../_lib/math", "node:fs");
        let error = load_one(&node, &[]).unwrap_err();
        assert!(
            format!("{error:#}").contains("may import only"),
            "{error:#}"
        );

        let value_import = USES_LIB.replace("import type { Ctx }", "import { Ctx }");
        let error = load_one(
            &value_import,
            &[("math", "export const twice = (n: number): number => n;")],
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("use `import type`"),
            "{error:#}"
        );
    }

    const ECHO: &str = r#"
import type { Ctx } from "../centaur";

/** Echo the args back. */
export async function echo(args: { text: string }, ctx: Ctx): Promise<{ text: string }> { return { text: args.text }; }
"#;

    fn write_connector(dir: &Path, description: &str) {
        let connector = dir.join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(
            connector.join(MANIFEST),
            format!("description = \"{description}\"\n"),
        )
        .unwrap();
        std::fs::write(connector.join(ENTRY), ECHO).unwrap();
    }

    #[test]
    fn a_missing_root_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("connectors");
        write_connector(&root, "Demo");
        let missing = dir.path().join("not-mounted");

        let catalog = Catalog::load(&[missing.clone(), root.clone()], true).unwrap();
        assert_eq!(catalog.connectors.len(), 1);
        assert_eq!(
            revision(std::slice::from_ref(&missing)).unwrap(),
            revision(&[]).unwrap(),
            "an unmounted root contributes no content"
        );
        assert!(
            Catalog::load(&[missing], true)
                .unwrap()
                .connectors
                .is_empty(),
            "startup emptiness is the caller's error to raise"
        );
    }

    #[test]
    fn the_revision_tracks_the_files_a_load_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("connectors");
        write_connector(&root, "Demo");
        let roots = std::slice::from_ref(&root);
        let before = revision(roots).unwrap();
        assert_eq!(before.len(), REVISION_CHARS);
        assert_eq!(before, revision(roots).unwrap(), "deterministic");

        // A file a load does not read does not matter.
        std::fs::write(root.join("demo/notes.md"), "not compiled").unwrap();
        assert_eq!(before, revision(roots).unwrap());

        write_connector(&root, "Changed");
        assert_ne!(before, revision(roots).unwrap());
    }

    #[test]
    fn rejects_invalid_exports() {
        let unannotated = compile(
            "export async function f(args: { a: string }) { return 1; }",
            Path::new("index.ts"),
            true,
        )
        .err()
        .unwrap();
        assert!(
            unannotated.to_string().contains("explicit annotations"),
            "{unannotated}"
        );

        let synchronous = compile(
            "export function f(args: { a: string }): number { return 1; }",
            Path::new("index.ts"),
            true,
        )
        .err()
        .unwrap();
        assert!(
            synchronous.to_string().contains("must be async"),
            "{synchronous}"
        );
    }

    #[test]
    fn the_policy_matches_names_exactly() {
        let policy = |allowlist: Option<&[&str]>, blocklist: &[&str]| Policy {
            allowlist: allowlist.map(|names| names.iter().map(|name| (*name).to_owned()).collect()),
            blocklist: Some(blocklist.iter().map(|name| (*name).to_owned()).collect()),
        };
        assert!(Policy::default().allows("slack"));
        let filter = policy(Some(&["slack"]), &["dune"]);
        assert!(filter.allows("slack") && !filter.allows("dune") && !filter.allows("linear"));
        assert!(
            !policy(Some(&["dune"]), &["dune"]).allows("dune"),
            "blocklist wins"
        );
        assert!(!policy(Some(&["google-docs"]), &[]).allows("google_docs"));
        assert!(!policy(Some(&["Slack"]), &[]).allows("slack"));
    }
}
