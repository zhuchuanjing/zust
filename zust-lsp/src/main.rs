use compiler::{Compiler, CompilerDiagnostic, SpannedCompilerError, Symbol};
use dashmap::DashMap;
use dynamic::{Dynamic, Type};
use parser::{Expr, ExprKind, Parser, Pattern, PatternKind, Span, Stmt, StmtKind};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug, Clone)]
struct SymbolInfo {
    name: String,
    kind: SymbolKind,
    range: Range,
    selection_range: Range,
    detail: String,
}

#[derive(Debug, Clone, Default)]
struct DocumentIndex {
    symbols: Vec<SymbolInfo>,
}

struct Backend {
    client: Client,
    documents: Arc<DashMap<Url, String>>,
    indexes: Arc<DashMap<Url, DocumentIndex>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                completion_provider: Some(CompletionOptions { resolve_provider: Some(false), trigger_characters: Some(vec![".".to_string(), ":".to_string()]), ..CompletionOptions::default() }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo { name: "zust-lsp".to_string(), version: Some(env!("CARGO_PKG_VERSION").to_string()) }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client.log_message(MessageType::INFO, "zust-lsp initialized").await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.insert(uri.clone(), params.text_document.text);
        self.refresh(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            let uri = params.text_document.uri;
            self.documents.insert(uri.clone(), change.text);
            self.refresh(uri).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.documents.insert(params.text_document.uri.clone(), text);
        }
        self.refresh(params.text_document.uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        self.indexes.remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let mut items = keyword_completions();
        if let Some(index) = self.indexes.get(&uri) {
            items.extend(index.symbols.iter().map(|symbol| CompletionItem { label: symbol.name.clone(), kind: Some(completion_kind(symbol.kind)), detail: Some(symbol.detail.clone()), ..CompletionItem::default() }));
        }
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let TextDocumentPositionParams { text_document, position } = params.text_document_position_params;
        let Some(text) = self.documents.get(&text_document.uri) else {
            return Ok(None);
        };
        let Some(word) = word_at(&text, position) else {
            return Ok(None);
        };
        let Some(symbol) = self.find_symbol(&text_document.uri, &word) else {
            return Ok(None);
        };
        Ok(Some(Hover { contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: format!("```zust\n{}\n```", symbol.detail) }), range: Some(symbol.selection_range) }))
    }

    async fn goto_definition(&self, params: GotoDefinitionParams) -> Result<Option<GotoDefinitionResponse>> {
        let TextDocumentPositionParams { text_document, position } = params.text_document_position_params;
        let Some(text) = self.documents.get(&text_document.uri) else {
            return Ok(None);
        };
        let Some(word) = word_at(&text, position) else {
            return Ok(None);
        };
        let Some(symbol) = self.find_symbol(&text_document.uri, &word) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location { uri: text_document.uri, range: symbol.selection_range })))
    }

    #[allow(deprecated)]
    async fn document_symbol(&self, params: DocumentSymbolParams) -> Result<Option<DocumentSymbolResponse>> {
        let Some(index) = self.indexes.get(&params.text_document.uri) else {
            return Ok(Some(DocumentSymbolResponse::Nested(Vec::new())));
        };
        let symbols = index
            .symbols
            .iter()
            .filter(|symbol| matches!(symbol.kind, SymbolKind::FUNCTION | SymbolKind::STRUCT | SymbolKind::CONSTANT | SymbolKind::VARIABLE))
            .map(|symbol| DocumentSymbol {
                name: symbol.name.clone(),
                detail: Some(symbol.detail.clone()),
                kind: symbol.kind,
                tags: None,
                deprecated: None,
                range: symbol.range,
                selection_range: symbol.selection_range,
                children: None,
            })
            .collect();
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }
}

impl Backend {
    async fn refresh(&self, uri: Url) {
        let Some(text) = self.documents.get(&uri).map(|doc| doc.clone()) else {
            return;
        };
        self.indexes.insert(uri.clone(), index_document(&text));
        let source_path = uri.to_file_path().ok();
        let diagnostics = diagnostics_for(&module_name(&uri), &text, source_path.as_deref());
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }

    fn find_symbol(&self, uri: &Url, name: &str) -> Option<SymbolInfo> {
        self.indexes.get(uri).and_then(|index| index.symbols.iter().find(|symbol| symbol.name == name).cloned())
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend { client, documents: Arc::new(DashMap::new()), indexes: Arc::new(DashMap::new()) });
    Server::new(stdin, stdout, socket).serve(service).await;
}

fn diagnostics_for(module: &str, text: &str, source_path: Option<&Path>) -> Vec<Diagnostic> {
    check_code_with_lsp_externs(module, text.as_bytes().to_vec(), source_path)
        .into_iter()
        .map(|diag| Diagnostic { range: range_for_span(text, diag.span), severity: Some(DiagnosticSeverity::ERROR), source: Some("zust".to_string()), message: diag.message, ..Diagnostic::default() })
        .collect()
}

fn check_code_with_lsp_externs(name: &str, code: Vec<u8>, source_path: Option<&Path>) -> Vec<CompilerDiagnostic> {
    let mut parser = Parser::new(code);
    let mut stmts = Vec::new();
    loop {
        match parser.stmt(false) {
            Ok(stmt) => stmts.push(stmt),
            Err(err) => {
                if parser.is_eof() {
                    break;
                }
                return vec![CompilerDiagnostic { message: format!("解析错误：{err:#}"), span: Span::empty(parser.current_pos()) }];
            }
        }
    }

    let mut compiler = Compiler::new();
    register_lsp_externs(&mut compiler);
    if let Some(diag) = register_lsp_imports(&mut compiler, &stmts, source_path) {
        return vec![diag];
    }
    compiler.clear();
    match compiler.compile(name.into(), stmts) {
        Ok(_) => Vec::new(),
        Err(err) => {
            if let Some(err) = err.downcast_ref::<SpannedCompilerError>() {
                vec![CompilerDiagnostic { message: err.message.clone(), span: err.span }]
            } else {
                vec![CompilerDiagnostic { message: format!("{err:#}"), span: Span::default() }]
            }
        }
    }
}

fn register_lsp_imports(compiler: &mut Compiler, stmts: &[Stmt], source_path: Option<&Path>) -> Option<CompilerDiagnostic> {
    let mut visited = BTreeSet::new();
    for stmt in stmts {
        let Some((module, path)) = lsp_import_decl(stmt) else {
            continue;
        };
        if !compiler.symbols.symbol(&module).is_empty() {
            continue;
        }
        let resolved =
            resolve_lsp_import_path(&path, source_path).map_err(|err| err.to_string()).and_then(|path| register_lsp_import_declarations(compiler, &module, &path, &mut visited).map_err(|err| format!("{err:#}")));
        if let Err(message) = resolved {
            return Some(CompilerDiagnostic { message: format!("导入 {module} 失败：{message}"), span: stmt.span });
        }
    }
    None
}

fn register_lsp_import_declarations(compiler: &mut Compiler, module: &str, path: &Path, visited: &mut BTreeSet<PathBuf>) -> std::result::Result<(), String> {
    let canonical = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let code = std::fs::read(&canonical).map_err(|err| err.to_string())?;
    let stmts = Compiler::parse_code(code).map_err(|err| format!("{err:#}"))?;

    compiler.symbols.add_module(module.into());
    for stmt in &stmts {
        register_lsp_import_decl_symbol(compiler, module, stmt);
    }
    compiler.symbols.pop_module();
    Ok(())
}

fn register_lsp_import_decl_symbol(compiler: &mut Compiler, module: &str, stmt: &Stmt) {
    match &stmt.kind {
        StmtKind::Fn { name, args, is_pub, .. } if *is_pub => {
            let (ty, _) = Type::from_args(args.clone());
            let _ = compiler.symbols.add_to_module(module, name.clone(), Symbol::Native(ty));
        }
        StmtKind::Struct { name, def, is_pub } if *is_pub => {
            let _ = compiler.symbols.add_to_module(module, name.clone(), Symbol::Struct(def.clone(), true));
        }
        StmtKind::Static { name, ty, is_pub, .. } if *is_pub => {
            let _ = compiler.symbols.add_to_module(module, name.clone(), Symbol::Static { value: None, ty: ty.clone(), is_pub: true });
        }
        StmtKind::Const { name, ty, is_pub, .. } if *is_pub => {
            let _ = compiler.symbols.add_to_module(module, name.clone(), Symbol::Static { value: None, ty: ty.clone(), is_pub: true });
        }
        StmtKind::Impl { target, body } => {
            register_lsp_import_impl_symbols(compiler, module, target, body);
        }
        _ => {}
    }
}

fn register_lsp_import_impl_symbols(compiler: &mut Compiler, module: &str, target: &Type, body: &Stmt) {
    let Some(target_name) = lsp_impl_target_name(target) else {
        return;
    };
    let StmtKind::Block(fns) = &body.kind else {
        return;
    };
    let struct_id = compiler.symbols.get_id(&format!("{module}::{target_name}")).ok();
    for stmt in fns {
        let StmtKind::Fn { name, args, is_pub, .. } = &stmt.kind else {
            continue;
        };
        if !*is_pub {
            continue;
        }
        let (ty, _) = Type::from_args(args.clone());
        let Ok(fn_id) = compiler.symbols.add_to_module(module, format!("{target_name}::{name}").into(), Symbol::Native(ty)) else {
            continue;
        };
        if let Some(struct_id) = struct_id
            && let Some((_, Symbol::Struct(struct_ty, _))) = compiler.symbols.get_symbol_mut(struct_id)
        {
            let _ = struct_ty.add_field(name.clone(), Type::Symbol { id: fn_id, params: Vec::new() });
        }
    }
}

fn lsp_impl_target_name(target: &Type) -> Option<String> {
    match target {
        Type::Ident { name, .. } => Some(name.to_string()),
        Type::Symbol { id, .. } => Some(id.to_string()),
        _ => None,
    }
}

fn lsp_import_decl(stmt: &Stmt) -> Option<(String, String)> {
    let StmtKind::Expr(expr, _) = &stmt.kind else {
        return None;
    };
    let ExprKind::Call { obj, params } = &expr.kind else {
        return None;
    };
    let ExprKind::Ident(name) = &obj.kind else {
        return None;
    };
    if name.as_str() != "import" {
        return None;
    }
    match params.as_slice() {
        [module, path] => Some((lsp_import_name(module)?, lsp_import_name(path)?)),
        [module] => {
            let module = lsp_import_name(module)?;
            Some((module.clone(), format!("{module}.zs")))
        }
        _ => None,
    }
}

fn lsp_import_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(name.to_string()),
        ExprKind::Value(Dynamic::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn resolve_lsp_import_path(path: &str, source_path: Option<&Path>) -> std::io::Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if let Some(base_dir) = source_path.and_then(Path::parent) {
        return Ok(base_dir.join(path));
    }
    std::env::current_dir().map(|cwd| cwd.join(path))
}

fn register_lsp_externs(compiler: &mut Compiler) {
    let mut modules = BTreeSet::new();
    for ext in lsp_externs() {
        let native = Symbol::native(ext.arg_tys, ext.ret_ty);
        if let Some((module, name)) = ext.full_name.split_once("::") {
            if modules.insert(module.to_string()) {
                compiler.symbols.add_module(module.into());
            }
            let _ = compiler.symbols.add_to_module(module, name.into(), native);
        } else {
            compiler.add_symbol(ext.full_name, native);
        }
    }
}

struct LspExtern {
    full_name: &'static str,
    arg_tys: Vec<Type>,
    ret_ty: Type,
}

fn lsp_externs() -> Vec<LspExtern> {
    vec![
        LspExtern { full_name: "std::print", arg_tys: vec![Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "std::import", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Bool },
        LspExtern { full_name: "std::uuid", arg_tys: vec![], ret_ty: Type::Any },
        LspExtern { full_name: "std::rand", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "std::__struct_alloc", arg_tys: vec![Type::I64], ret_ty: Type::Any },
        LspExtern { full_name: "std::__struct_from_ptr", arg_tys: vec![Type::I64, Type::I64], ret_ty: Type::Any },
        LspExtern { full_name: "std::log", arg_tys: vec![Type::F32], ret_ty: Type::F32 },
        LspExtern { full_name: "spirv::group_id", arg_tys: vec![], ret_ty: Type::Vec(Rc::new(Type::U32), 3) },
        LspExtern { full_name: "spirv::local_id", arg_tys: vec![], ret_ty: Type::Vec(Rc::new(Type::U32), 3) },
        LspExtern { full_name: "spirv::barrier", arg_tys: vec![], ret_ty: Type::Void },
        LspExtern { full_name: "spirv::atomic_add", arg_tys: vec![Type::U32, Type::U32], ret_ty: Type::U32 },
        LspExtern { full_name: "root::mount", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "root::add_list", arg_tys: vec![Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "root::add_map", arg_tys: vec![Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "root::add", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::I32 },
        LspExtern { full_name: "root::dir", arg_tys: vec![Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::remove", arg_tys: vec![Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::contains", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::I32 },
        LspExtern { full_name: "root::send", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::send_idx", arg_tys: vec![Type::Any, Type::I64, Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "root::get", arg_tys: vec![Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::len", arg_tys: vec![Type::Any], ret_ty: Type::I64 },
        LspExtern { full_name: "root::push", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::I64 },
        LspExtern { full_name: "root::get_idx", arg_tys: vec![Type::Any, Type::I64], ret_ty: Type::Any },
        LspExtern { full_name: "root::remove_idx", arg_tys: vec![Type::Any, Type::I64], ret_ty: Type::Any },
        LspExtern { full_name: "root::insert", arg_tys: vec![Type::Any, Type::Any, Type::Any], ret_ty: Type::Void },
        LspExtern { full_name: "root::get_key", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::remove_key", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "root::add_fn", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Bool },
        LspExtern { full_name: "llm::complete", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "llm::image", arg_tys: vec![Type::Any, Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "llm::audio", arg_tys: vec![Type::Any, Type::Any], ret_ty: Type::Any },
        LspExtern { full_name: "llm::deep", arg_tys: vec![Type::Any, Type::Any, Type::Any], ret_ty: Type::Any },
    ]
}

fn index_document(text: &str) -> DocumentIndex {
    let Ok(stmts) = Compiler::parse_code(text.as_bytes().to_vec()) else {
        return DocumentIndex::default();
    };
    let mut index = DocumentIndex::default();
    for stmt in &stmts {
        collect_stmt_symbols(text, stmt, &mut index.symbols);
    }
    index
}

fn collect_stmt_symbols(text: &str, stmt: &Stmt, symbols: &mut Vec<SymbolInfo>) {
    match &stmt.kind {
        StmtKind::Fn { name, generic_params, args, body, is_pub } => {
            let generic_detail = if generic_params.is_empty() { String::new() } else { format!("<{}>", generic_params.iter().map(type_label).collect::<Vec<_>>().join(", ")) };
            let detail = format!("{}fn {}{}({})", if *is_pub { "pub " } else { "" }, name, generic_detail, args.iter().map(|(name, ty)| format!("{name}: {}", type_label(ty))).collect::<Vec<_>>().join(", "));
            push_named_symbol(text, symbols, name.as_str(), SymbolKind::FUNCTION, stmt.span, &["pub", "fn"], detail);
            collect_stmt_symbols(text, body, symbols);
        }
        StmtKind::Struct { name, def, is_pub } => {
            let detail = format!("{}struct {} {def:?}", if *is_pub { "pub " } else { "" }, name);
            push_named_symbol(text, symbols, name.as_str(), SymbolKind::STRUCT, stmt.span, &["pub", "struct"], detail);
        }
        StmtKind::Const { name, ty, is_pub, .. } => {
            let detail = format!("{}const {}: {ty:?}", if *is_pub { "pub " } else { "" }, name);
            push_named_symbol(text, symbols, name.as_str(), SymbolKind::CONSTANT, stmt.span, &["pub", "const"], detail);
        }
        StmtKind::Static { name, ty, is_pub, .. } => {
            let detail = format!("{}static {}: {ty:?}", if *is_pub { "pub " } else { "" }, name);
            push_named_symbol(text, symbols, name.as_str(), SymbolKind::VARIABLE, stmt.span, &["pub", "static"], detail);
        }
        StmtKind::Let { pat, value } => {
            collect_pattern_symbols(text, pat, symbols);
            collect_stmt_symbols(text, value, symbols);
        }
        StmtKind::Block(stmts) => {
            for stmt in stmts {
                collect_stmt_symbols(text, stmt, symbols);
            }
        }
        StmtKind::If { cond, then_body, else_body } => {
            collect_expr_symbols(text, cond, symbols);
            collect_stmt_symbols(text, then_body, symbols);
            if let Some(else_body) = else_body {
                collect_stmt_symbols(text, else_body, symbols);
            }
        }
        StmtKind::While { cond, body } => {
            collect_expr_symbols(text, cond, symbols);
            collect_stmt_symbols(text, body, symbols);
        }
        StmtKind::For { pat, range, body } => {
            collect_pattern_symbols(text, pat, symbols);
            collect_expr_symbols(text, range, symbols);
            collect_stmt_symbols(text, body, symbols);
        }
        StmtKind::Loop(body) => collect_stmt_symbols(text, body, symbols),
        StmtKind::Expr(expr, _) => collect_expr_symbols(text, expr, symbols),
        StmtKind::Return(Some(expr)) => collect_expr_symbols(text, expr, symbols),
        StmtKind::Impl { target, body } => {
            let name = impl_target_symbol_name(target);
            let detail = format!("impl {target:?}");
            push_named_symbol(text, symbols, name.as_str(), SymbolKind::OBJECT, stmt.span, &["impl"], detail);
            collect_stmt_symbols(text, body, symbols);
        }
        StmtKind::Break | StmtKind::Continue | StmtKind::Return(None) => {}
    }
}

fn type_label(ty: &Type) -> String {
    match ty {
        Type::Any => "Any".to_string(),
        Type::Void => "void".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Str => "string".to_string(),
        Type::Map => "Map".to_string(),
        Type::List => "List".to_string(),
        Type::Iter => "Iter".to_string(),
        Type::I8 => "i8".to_string(),
        Type::I16 => "i16".to_string(),
        Type::I32 => "i32".to_string(),
        Type::I64 => "i64".to_string(),
        Type::U8 => "u8".to_string(),
        Type::U16 => "u16".to_string(),
        Type::U32 => "u32".to_string(),
        Type::U64 => "u64".to_string(),
        Type::F16 => "f16".to_string(),
        Type::F32 => "f32".to_string(),
        Type::F64 => "f64".to_string(),
        Type::ConstInt(value) => value.to_string(),
        Type::Ident { name, params } => type_name_label(name, params),
        Type::Symbol { id, params } => type_name_label(&id.to_string(), params),
        Type::Vec(elem, 0) => format!("Vec<{}>", type_label(elem)),
        Type::Vec(elem, len) => format!("{}{}", type_label(elem), len),
        Type::Array(elem, len) => format!("[{}; {len}]", type_label(elem)),
        Type::ArrayParam(elem, len) => format!("[{}; {}]", type_label(elem), type_label(len)),
        Type::Tuple(items) => format!("({})", items.iter().map(type_label).collect::<Vec<_>>().join(", ")),
        Type::Struct { params, .. } => type_name_label("struct", params),
        Type::Fn { tys, ret } => format!("fn({}) -> {}", tys.iter().map(type_label).collect::<Vec<_>>().join(", "), type_label(ret)),
        other => format!("{other:?}"),
    }
}

fn type_name_label(name: &str, params: &[Type]) -> String {
    if params.is_empty() { name.to_string() } else { format!("{name}<{}>", params.iter().map(type_label).collect::<Vec<_>>().join(", ")) }
}

fn impl_target_symbol_name(target: &Type) -> String {
    match target {
        Type::Ident { name, .. } => name.to_string(),
        other => format!("{other:?}"),
    }
}

fn collect_pattern_symbols(text: &str, pat: &Pattern, symbols: &mut Vec<SymbolInfo>) {
    match &pat.kind {
        PatternKind::Ident { name, ty } => {
            symbols.push(SymbolInfo { name: name.to_string(), kind: SymbolKind::VARIABLE, range: range_for_span(text, pat.span), selection_range: range_for_span(text, pat.span), detail: format!("let {name}: {ty:?}") });
        }
        PatternKind::Tuple(items) => {
            for item in items {
                collect_pattern_symbols(text, item, symbols);
            }
        }
        PatternKind::List { elems, .. } => {
            for item in elems {
                collect_pattern_symbols(text, item, symbols);
            }
        }
        PatternKind::Member(inner, _) | PatternKind::Idx(inner, _) => collect_pattern_symbols(text, inner, symbols),
        PatternKind::Wildcard | PatternKind::Var { .. } | PatternKind::Literal(_) => {}
    }
}

fn collect_expr_symbols(text: &str, expr: &Expr, symbols: &mut Vec<SymbolInfo>) {
    match &expr.kind {
        ExprKind::Closure { args, body } => {
            for (name, ty) in args {
                if let Some(span) = find_name_span(text, expr.span, name, &["|"]) {
                    symbols.push(SymbolInfo {
                        name: name.to_string(),
                        kind: SymbolKind::VARIABLE,
                        range: range_for_span(text, span),
                        selection_range: range_for_span(text, span),
                        detail: format!("closure parameter {name}: {ty:?}"),
                    });
                }
            }
            collect_stmt_symbols(text, body, symbols);
        }
        ExprKind::Typed { value, .. } | ExprKind::Unary { value, .. } | ExprKind::Repeat { value, .. } => collect_expr_symbols(text, value, symbols),
        ExprKind::Binary { left, right, .. } => {
            collect_expr_symbols(text, left, symbols);
            collect_expr_symbols(text, right, symbols);
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for item in items {
                collect_expr_symbols(text, item, symbols);
            }
        }
        ExprKind::Dict(items) => {
            for (_, value) in items {
                collect_expr_symbols(text, value, symbols);
            }
        }
        ExprKind::Range { start, stop, .. } => {
            collect_expr_symbols(text, start, symbols);
            collect_expr_symbols(text, stop, symbols);
        }
        ExprKind::Call { obj, params } => {
            collect_expr_symbols(text, obj, symbols);
            for param in params {
                collect_expr_symbols(text, param, symbols);
            }
        }
        ExprKind::Stmt(stmt) => collect_stmt_symbols(text, stmt, symbols),
        ExprKind::Null | ExprKind::Value(_) | ExprKind::Const(_) | ExprKind::Ident(_) | ExprKind::Var(_) | ExprKind::Capture(_) | ExprKind::Id(_, _) | ExprKind::Assoc { .. } | ExprKind::AssocId { .. } => {}
    }
}

fn push_named_symbol(text: &str, symbols: &mut Vec<SymbolInfo>, name: &str, kind: SymbolKind, span: Span, prefixes: &[&str], detail: String) {
    let selection_span = find_name_span(text, span, name, prefixes).unwrap_or(span);
    symbols.push(SymbolInfo { name: name.to_string(), kind, range: range_for_span(text, span), selection_range: range_for_span(text, selection_span), detail });
}

fn find_name_span(text: &str, span: Span, name: &str, prefixes: &[&str]) -> Option<Span> {
    let start = span.start.min(text.len());
    let end = span.end.min(text.len());
    let bytes = text.as_bytes();
    let mut pos = start;
    for prefix in prefixes {
        while pos < end && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if text.get(pos..end)?.starts_with(prefix) {
            pos += prefix.len();
        }
    }
    text.get(pos..end)?.find(name).map(|offset| Span::new(pos + offset, pos + offset + name.len()))
}

fn range_for_span(text: &str, span: Span) -> Range {
    let start = position_for_offset(text, span.start);
    let mut end = position_for_offset(text, span.end);
    if end == start {
        end.character += 1;
    }
    Range { start, end }
}

fn position_for_offset(text: &str, offset: usize) -> Position {
    let target = offset.min(text.len());
    let mut line = 0;
    let mut line_start = 0;
    for (idx, ch) in text.char_indices() {
        if idx >= target {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = idx + ch.len_utf8();
        }
    }
    let character = text[line_start..target].encode_utf16().count() as u32;
    Position::new(line, character)
}

fn offset_for_position(text: &str, position: Position) -> usize {
    let mut line = 0;
    let mut line_start = 0;
    for (idx, ch) in text.char_indices() {
        if line == position.line {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = idx + ch.len_utf8();
        }
    }
    let line_end = text[line_start..].find('\n').map(|offset| line_start + offset).unwrap_or(text.len());
    let mut utf16 = 0;
    for (idx, ch) in text[line_start..line_end].char_indices() {
        if utf16 >= position.character {
            return line_start + idx;
        }
        utf16 += ch.len_utf16() as u32;
    }
    line_end
}

fn word_at(text: &str, position: Position) -> Option<String> {
    let offset = offset_for_position(text, position).min(text.len());
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    (start < end).then(|| text[start..end].to_string())
}

fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn module_name(uri: &Url) -> String {
    uri.path_segments().and_then(|mut segments| segments.next_back()).and_then(|file| file.strip_suffix(".zs")).filter(|name| !name.is_empty()).unwrap_or("main").replace('-', "_")
}

fn keyword_completions() -> Vec<CompletionItem> {
    [
        "pub", "fn", "struct", "impl", "let", "const", "static", "if", "else", "for", "in", "while", "loop", "return", "break", "continue", "true", "false", "null", "bool", "string", "i8", "i16", "i32", "i64", "u8",
        "u16", "u32", "u64", "f32", "f64",
    ]
    .into_iter()
    .map(|label| CompletionItem { label: label.to_string(), kind: Some(CompletionItemKind::KEYWORD), ..CompletionItem::default() })
    .collect()
}

fn completion_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::FUNCTION => CompletionItemKind::FUNCTION,
        SymbolKind::STRUCT => CompletionItemKind::STRUCT,
        SymbolKind::CONSTANT => CompletionItemKind::CONSTANT,
        SymbolKind::VARIABLE => CompletionItemKind::VARIABLE,
        _ => CompletionItemKind::TEXT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_byte_offsets_to_lsp_utf16_positions() {
        let text = "pub fn main() {\n  let 值 = 1;\n}\n";
        let byte_offset = text.find("值").expect("identifier exists");
        let position = position_for_offset(text, byte_offset);

        assert_eq!(position, Position::new(1, 6));
        assert_eq!(offset_for_position(text, position), byte_offset);
    }

    #[test]
    fn indexes_top_level_and_local_symbols() {
        let text = "pub struct Point { x: i32, y: i32 }\n\npub fn main() {\n  let answer = 42;\n  answer\n}\n";
        let index = index_document(text);

        assert!(index.symbols.iter().any(|symbol| symbol.name == "Point" && symbol.kind == SymbolKind::STRUCT));
        assert!(index.symbols.iter().any(|symbol| symbol.name == "main" && symbol.kind == SymbolKind::FUNCTION));
        assert!(index.symbols.iter().any(|symbol| symbol.name == "answer" && symbol.kind == SymbolKind::VARIABLE));
    }

    #[test]
    fn indexes_generic_function_symbols() {
        let text = "pub struct Params<N> { value: [u32; N] }\n\npub fn main<N>(params: Params<N>, buf: Vec<f32>) {\n  let first = params.value[0]\n}\n";
        let index = index_document(text);
        let main = index.symbols.iter().find(|symbol| symbol.name == "main" && symbol.kind == SymbolKind::FUNCTION).expect("generic main symbol");

        assert_eq!(main.detail, "pub fn main<N>(params: Params<N>, buf: Vec<f32>)");
        assert!(index.symbols.iter().any(|symbol| symbol.name == "first" && symbol.kind == SymbolKind::VARIABLE));
    }

    #[test]
    fn reports_compiler_diagnostics_with_span_ranges() {
        let text = "pub fn main() {\n  missing_name\n}\n";
        let diagnostics = diagnostics_for("main", text, None);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].range.start, Position::new(1, 2));
        assert!(diagnostics[0].message.contains("missing_name"));
    }

    #[test]
    fn accepts_registered_spirv_externs() {
        let text = "pub fn main() {\n  let group = spirv::group_id();\n  let local = spirv::local_id();\n  spirv::barrier();\n  group[0] + local[0]\n}\n";
        let diagnostics = diagnostics_for("main", text, None);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_generic_function_syntax() {
        let text = "pub struct Params<N> { value: [u32; N] }\n\npub fn main<N>(params: Params<N>) {\n  params.value[0]\n}\n";
        let diagnostics = diagnostics_for("generic_main", text, None);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_generic_mandelbrot_entry() {
        let text = include_str!("../../zusts/gpu/mandelbrot.zs");
        let source_path = workspace_root().join("zusts/gpu/mandelbrot.zs");
        let diagnostics = diagnostics_for("mandelbrot", text, Some(&source_path));

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn registers_std_runtime_functions_as_unqualified_roots() {
        let mut compiler = Compiler::new();
        register_lsp_externs(&mut compiler);

        let id = compiler.symbols.get_id("rand").expect("rand should resolve through the std root");
        let (name, symbol) = compiler.symbols.get_symbol(id).expect("rand symbol should exist");
        assert_eq!(name.as_str(), "std::rand");
        assert!(symbol.is_fn(), "{symbol:?}");
    }

    #[test]
    fn accepts_std_runtime_functions() {
        let text = "pub fn main() {\n  let x = rand(-1.0, 1.0);\n  print(x);\n  uuid()\n}\n";
        let diagnostics = diagnostics_for("test", text, None);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_project_test_module_std_rand_calls() {
        let text = include_str!("../../zusts/test.zs");
        let source_path = workspace_root().join("zusts/test.zs");
        let diagnostics = diagnostics_for("test", text, Some(&source_path));

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_dynamic_any_field_and_method_access() {
        let text = "pub fn read_user(req) {\n  req.user.profile.name\n}\n";
        let diagnostics = diagnostics_for("test2", text, None);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_runtime_root_module_calls() {
        let text = "pub fn record_event() {\n  root::push(\"local/events\", {id: uuid()});\n}\n";
        let source_path = workspace_root().join("zusts/bug_tests/root_module_calls.zs");
        let diagnostics = diagnostics_for("root_module_calls", text, Some(&source_path));

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn resolves_imports_relative_to_current_file() {
        let text = "import(\"bigfloat\", \"../bigfloat.zs\");\npub fn make_bigfloat() {\n  bigfloat::BigFloat<2>::from_u32(1u32)\n}\n";
        let source_path = workspace_root().join("zusts/gpu/import_check.zs");
        let diagnostics = diagnostics_for("record", text, Some(&source_path));

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn accepts_imported_generic_associated_functions() {
        for file in ["mandelbrot_bigfloat2.zs", "mandelbrot_bigfloat4.zs", "mandelbrot_bigfloat8.zs"] {
            let source_path = workspace_root().join("zusts/gpu").join(file);
            let text = std::fs::read_to_string(&source_path).expect("read BigFloat GPU example");
            let module = file.strip_suffix(".zs").expect("zust file");
            let diagnostics = diagnostics_for(module, &text, Some(&source_path));

            assert!(diagnostics.is_empty(), "{file}: {diagnostics:?}");
        }
    }

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root").to_path_buf()
    }
}
