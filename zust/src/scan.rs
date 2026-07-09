//! source::scan_repo 实现:扫描源码目录,按文件分组返回语法单元。
//!
//! 设计原则:
//! - tree-sitter 解析在 Rust 内部完成,不向 zust 暴露 node 原始接口。
//! - 每个文件返回 `{path, language, loc, units: [{kind, name, span, ...}]}`。
//! - 主流语言(Rust/C/C++/JS/TS/Go/Python/Ruby/PHP/Bash/Perl/Lua/Tcl/Java/Kotlin/C#/GDScript/GLSL/WGSL)
//!   全部覆盖,扩展名识别 + tree-sitter grammar 完整。
//! - skip 通用非源码目录(target/、node_modules/、.git/、vendor/、dist/ 等)
//!   减少无关文件被解析的浪费。

use anyhow::Result;
use dynamic::Dynamic;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

pub fn supported_languages() -> &'static [&'static str] {
    &[
        "Rust", "C", "Cpp", "JavaScript", "TypeScript", "TSX", "Go", "Python",
        "Ruby", "Php", "Shell", "Bash", "Perl", "Lua", "Tcl", "Java", "Kotlin",
        "CSharp", "GDScript", "GLSL", "WGSL", "Swift", "Scala", "Haskell", "Elixir",
        "OCaml", "Zig", "R", "SQL", "HTML", "CSS", "JSON", "YAML", "TOML", "Markdown",
        "Dockerfile", "Erlang", "FSharp", "D", "Zust",
    ]
}

pub fn scan_repo(root: &Path, project: &str, run_id: &str) -> Result<Dynamic> {
    let t0 = Instant::now();
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut tasks: Vec<ScanTask> = Vec::new();
    for entry in walkdir::WalkDir::new(&root_canon)
        .into_iter()
        .filter_entry(|e| !is_excluded(&root_canon, e.path()))
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some((language, parser)) = language_for(entry.path()) else {
            continue;
        };
        let rel = relative_path(&root_canon, entry.path());
        tasks.push(ScanTask {
            path: rel,
            language,
            parser,
        });
    }

    let total_files = tasks.len();
    let mut files: Vec<ScannedFile> = Vec::with_capacity(tasks.len());
    let mut language_files: BTreeMap<String, usize> = BTreeMap::new();
    let mut parser_failed: usize = 0;
    for task in tasks {
        // task.path 是相对路径(给 files map 做 key),但 fs::read 需要用绝对路径
        // ——process CWD 不一定是 scan root,相对路径解析会指向错位置。
        let abs_path = root_canon.join(&task.path);
        *language_files.entry(task.language.label().to_string()).or_insert(0) += 1;
        let bytes = match std::fs::read(&abs_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&bytes).to_string();
        let loc = text.lines().count().max(1);
        let mut parser_obj = Parser::new();
        if parser_obj.set_language(&task.parser.lang).is_err() {
            parser_failed += 1;
            continue;
        }
        let tree = match parser_obj.parse(&text, None) {
            Some(t) => t,
            None => {
                parser_failed += 1;
                continue;
            }
        };
        let units = harvest_units(tree.root_node(), &text, task.language);
        files.push(ScannedFile {
            path: task.path,
            language: task.language.label().to_string(),
            loc,
            units,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut files_map: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    for f in &files {
        let unit_dyns: Vec<Dynamic> = f.units.iter().map(unit_dynamic).collect();
        let entry = dynamic_map([
            ("language", Dynamic::from(f.language.as_str())),
            ("loc", Dynamic::from(f.loc as u64)),
            ("units", Dynamic::list(unit_dyns)),
        ]);
        files_map.insert(f.path.clone().into(), entry);
    }

    let lang_labels: Vec<Dynamic> = language_files
        .iter()
        .map(|(k, v)| {
            let mut m = BTreeMap::new();
            m.insert(smol_str::SmolStr::new("language"), Dynamic::from(k.as_str()));
            m.insert(smol_str::SmolStr::new("count"), Dynamic::from(*v as u64));
            Dynamic::map(m)
        })
        .collect();

    let parser_failed_dynamic = Dynamic::from(parser_failed as u64);
    let total_dynamic = Dynamic::from(total_files as u64);
    let elapsed_ms = t0.elapsed().as_millis() as u64;

    Ok(dynamic_map([
        ("ok", Dynamic::Bool(true)),
        ("project_id", Dynamic::from(project.to_string())),
        ("run_id", Dynamic::from(run_id.to_string())),
        ("repo_root", Dynamic::from(root.to_string_lossy().to_string())),
        ("total_files", total_dynamic),
        ("parser_failed_files", parser_failed_dynamic),
        ("elapsed_ms", Dynamic::from(elapsed_ms)),
        ("language_files", Dynamic::list(lang_labels)),
        ("files", Dynamic::map(files_map)),
    ]))
}

struct ScanTask {
    path: String,
    language: Language,
    parser: LangEntry,
}

struct ScannedFile {
    path: String,
    language: String,
    loc: usize,
    units: Vec<SyntaxUnit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    C,
    Cpp,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Python,
    Ruby,
    Php,
    Shell,
    Perl,
    Lua,
    Tcl,
    Java,
    Kotlin,
    CSharp,
    GDScript,
    Glsl,
    Wgsl,
    Swift,
    Scala,
    Haskell,
    Elixir,
    OCaml,
    Zig,
    R,
    SQL,
    HTML,
    CSS,
    JSON,
    YAML,
    TOML,
    Markdown,
    Dockerfile,
    Erlang,
    FSharp,
    D,
    Zust,
}

impl Language {
    fn label(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::C => "C",
            Language::Cpp => "Cpp",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Tsx => "TSX",
            Language::Go => "Go",
            Language::Python => "Python",
            Language::Ruby => "Ruby",
            Language::Php => "Php",
            Language::Shell => "Shell",
            Language::Perl => "Perl",
            Language::Lua => "Lua",
            Language::Tcl => "Tcl",
            Language::Java => "Java",
            Language::Kotlin => "Kotlin",
            Language::CSharp => "CSharp",
            Language::GDScript => "GDScript",
            Language::Glsl => "GLSL",
            Language::Wgsl => "WGSL",
            Language::Swift => "Swift",
            Language::Scala => "Scala",
            Language::Haskell => "Haskell",
            Language::Elixir => "Elixir",
            Language::OCaml => "OCaml",
            Language::Zig => "Zig",
            Language::R => "R",
            Language::SQL => "SQL",
            Language::HTML => "HTML",
            Language::CSS => "CSS",
            Language::JSON => "JSON",
            Language::YAML => "YAML",
            Language::TOML => "TOML",
            Language::Markdown => "Markdown",
            Language::Dockerfile => "Dockerfile",
            Language::Erlang => "Erlang",
            Language::FSharp => "FSharp",
            Language::D => "D",
            Language::Zust => "Zust",
        }
    }
}

struct LangEntry {
    lang: tree_sitter::Language,
}

fn language_for(path: &Path) -> Option<(Language, LangEntry)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let base = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if base == "makefile" || base == "gnumakefile" {
        return None;
    }
    let lang = match ext.as_str() {
        "rs" => Language::Rust,
        "c" | "h" | "m" => Language::C,
        "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "inl" | "inc" | "ipp"
        | "mm" => Language::Cpp,
        "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
        "ts" => Language::TypeScript,
        "tsx" => Language::Tsx,
        "go" => Language::Go,
        "py" | "pyi" | "pyw" => Language::Python,
        "rb" | "rake" | "gemspec" => Language::Ruby,
        "php" => Language::Php,
        "sh" | "bash" => Language::Shell,
        "pl" | "pm" => Language::Perl,
        "lua" => Language::Lua,
        "tcl" => Language::Tcl,
        "java" => Language::Java,
        "kt" | "kts" => Language::Kotlin,
        "cs" => Language::CSharp,
        "gd" => Language::GDScript,
        "glsl" | "vert" | "frag" | "geom" | "comp" | "shader" => Language::Glsl,
        "wgsl" => Language::Wgsl,
        "swift" => Language::Swift,
        "scala" | "sc" => Language::Scala,
        "hs" => Language::Haskell,
        "ex" | "exs" => Language::Elixir,
        "ml" | "mli" => Language::OCaml,
        "zig" => Language::Zig,
        "r" | "R" => Language::R,
        "sql" => Language::SQL,
        "html" | "htm" => Language::HTML,
        "css" | "scss" | "less" => Language::CSS,
        "json" => Language::JSON,
        "yaml" | "yml" => Language::YAML,
        "toml" => Language::TOML,
        "md" | "markdown" => Language::Markdown,
        "dockerfile" => Language::Dockerfile,
        "erl" | "hrl" => Language::Erlang,
        "fs" | "fsi" | "fsx" => Language::FSharp,
        "d" => Language::D,
        "zs" | "zust" => Language::Zust,
        _ => return None,
    };
    let parser = lang_entry(lang)?;
    Some((lang, parser))
}

fn lang_entry(lang: Language) -> Option<LangEntry> {
    // 注意:几个 grammar 的 API 形态不一致,统一用 `.into()` 收口成 `tree_sitter::Language`:
    //   老 API (Rust/C/Go/Python/...)          -> LANGUAGE 常量,直接是 Language,identity into
    //   tree-sitter-language 0.1 (新 API)      -> LANGUAGE 常量是 LanguageFn,需要 .into()
    //   tree-sitter-markdown-updated 0.1       -> language() 函数直接返回 Language,identity into
    let lang_ts: tree_sitter::Language = match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::C => tree_sitter_c::LANGUAGE.into(),
        Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Language::Shell => tree_sitter_bash::LANGUAGE.into(),
        Language::Perl => tree_sitter_perl::LANGUAGE.into(),
        Language::Lua => tree_sitter_lua::LANGUAGE.into(),
        Language::Tcl => tree_sitter_tcl::LANGUAGE.into(),
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Language::GDScript => tree_sitter_gdscript::LANGUAGE.into(),
        Language::Glsl => tree_sitter_glsl::LANGUAGE_GLSL.into(),
        Language::Wgsl => tree_sitter_wgsl::LANGUAGE.into(),
        Language::Swift => tree_sitter_swift::LANGUAGE.into(),
        Language::Scala => tree_sitter_scala::LANGUAGE.into(),
        Language::Haskell => tree_sitter_haskell::LANGUAGE.into(),
        Language::Elixir => tree_sitter_elixir::LANGUAGE.into(),
        Language::OCaml => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        Language::Zig => tree_sitter_zig::LANGUAGE.into(),
        Language::R => tree_sitter_r::LANGUAGE.into(),
        Language::SQL => tree_sitter_sequel::LANGUAGE.into(),
        Language::HTML => tree_sitter_html::LANGUAGE.into(),
        Language::CSS => tree_sitter_css::LANGUAGE.into(),
        Language::JSON => tree_sitter_json::LANGUAGE.into(),
        Language::YAML => tree_sitter_yaml::LANGUAGE.into(),
        Language::TOML => tree_sitter_toml_ng::LANGUAGE.into(),
        Language::Markdown => tree_sitter_markdown_updated::language().into(),
        Language::Dockerfile => tree_sitter_containerfile::LANGUAGE.into(),
        Language::Erlang => tree_sitter_erlang::LANGUAGE.into(),
        Language::FSharp => tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        Language::D => tree_sitter_d::LANGUAGE.into(),
        Language::Zust => return None, // zust 走自家 parser,不在这里处理
    };
    Some(LangEntry {
        lang: lang_ts,
    })
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_excluded(root: &Path, path: &Path) -> bool {
    // 只比对 scan root 之下的相对 segments,这样:
    // - /abs/vendor/fd 扫描时,fd 是 root,vendor/ 是 root 之上,不算排除项
    // - root 是 /abs/proj,proj/vendor 才会被排除
    let rel = path.strip_prefix(root).unwrap_or(path);
    let lower = rel.to_string_lossy().to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    for seg in &segments {
        if matches!(
            *seg,
            "node_modules"
                | "vendor"
                | "thirdparty"
                | "third_party"
                | "third-party"
                | "deps"
                | ".yarn"
                | ".pnpm-store"
                | ".git"
                | ".github"
                | ".circleci"
                | ".buildkite"
                | ".devcontainer"
                | ".vscode"
                | ".idea"
                | "target"
                | "dist"
                | "coverage"
                | "autom4te.cache"
                | "build"
                | "cmake-build-debug"
                | "cmake-build-release"
        ) {
            return true;
        }
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        name.as_str(),
        "configure"
            | "configure.ac"
            | "configure.in"
            | "autogen.sh"
            | "bootstrap.sh"
            | "release.sh"
            | "install-sh"
            | "missing"
            | "depcomp"
            | "compile"
            | "config.guess"
            | "config.sub"
            | "config.rpath"
            | "ltmain.sh"
    ) {
        return true;
    }
    false
}

// ---- 语法单元抽取 ----

#[derive(Debug, Clone)]
struct SyntaxUnit {
    kind: String,
    name: Option<String>,
    start_line: u32,
    end_line: u32,
    is_public: bool,
    parent: Option<String>,
}

fn harvest_units(root: tree_sitter::Node<'_>, text: &str, language: Language) -> Vec<SyntaxUnit> {
    let mut out = Vec::new();
    walk(root, text, &mut out, language, None);
    out
}

fn walk(
    node: tree_sitter::Node<'_>,
    text: &str,
    out: &mut Vec<SyntaxUnit>,
    language: Language,
    parent_name: Option<&str>,
) {
    let kind = node.kind();
    if let Some((unit_kind, name, is_public)) = classify(kind, node, text, language) {
        out.push(SyntaxUnit {
            kind: unit_kind.to_string(),
            name: name.clone(),
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            is_public,
            parent: parent_name.map(|s| s.to_string()),
        });
        // 递归到子节点(让 function-body 内的 class 也能被识别),
        // 但函数/类已经识别过的子树就不再重复"用整个 span 拍一个 module"
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                walk(child, text, out, language, name.as_deref());
            }
        }
    } else {
        // 不识别为 unit 的子树,继续下钻
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                walk(child, text, out, language, parent_name);
            }
        }
    }
}

fn classify(
    kind: &str,
    node: tree_sitter::Node<'_>,
    text: &str,
    language: Language,
) -> Option<(&'static str, Option<String>, bool)> {
    let unit_kind = match language {
        Language::Rust => rust_kind(kind),
        Language::C | Language::Cpp => c_kind(kind),
        Language::JavaScript | Language::TypeScript | Language::Tsx => js_kind(kind),
        Language::Go => go_kind(kind),
        Language::Python => python_kind(kind),
        Language::Ruby => ruby_kind(kind),
        Language::Php => php_kind(kind),
        Language::Shell | Language::Perl | Language::Lua | Language::Tcl => script_kind(kind),
        Language::Java => java_kind(kind),
        Language::Kotlin => kotlin_kind(kind),
        Language::CSharp => csharp_kind(kind),
        Language::GDScript => gdscript_kind(kind),
        Language::Glsl | Language::Wgsl => glsl_kind(kind),
        Language::Swift => swift_kind(kind),
        Language::Scala => scala_kind(kind),
        Language::Haskell => haskell_kind(kind),
        Language::Elixir => elixir_kind(kind),
        Language::OCaml => ocaml_kind(kind),
        Language::Zig => zig_kind(kind),
        Language::R => r_kind(kind),
        Language::SQL => sql_kind(kind),
        Language::HTML => html_kind(kind),
        Language::CSS => css_kind(kind),
        Language::JSON => json_kind(kind),
        Language::YAML => yaml_kind(kind),
        Language::TOML => toml_kind(kind),
        Language::Markdown => markdown_kind(kind),
        Language::Dockerfile => dockerfile_kind(kind),
        Language::Erlang => erlang_kind(kind),
        Language::FSharp => fsharp_kind(kind),
        Language::D => d_kind(kind),
        Language::Zust => zust_kind(kind),
    };
    let unit_kind = unit_kind?;
    let name = extract_name(node, text);
    let is_public = detect_public(node, text, kind, language);
    Some((unit_kind, name, is_public))
}

fn rust_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" => Some("Function"),
        "impl_item" => Some("ImplBlock"),
        "struct_item" => Some("Struct"),
        "enum_item" => Some("Enum"),
        "trait_item" => Some("Trait"),
        "type_item" => Some("TypeAlias"),
        "mod_item" => Some("Module"),
        _ => None,
    }
}

fn c_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" | "declaration" | "function_declarator" => Some("Function"),
        "struct_specifier" | "union_specifier" => Some("StructOrUnion"),
        "enum_specifier" => Some("Enum"),
        "class_specifier" | "struct_or_union_specifier" => Some("Type"),
        "preproc_def" | "preproc_function_def" => Some("Macro"),
        "namespace_definition" => Some("Namespace"),
        _ => None,
    }
}

fn js_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "function" | "function_expression" | "arrow_function"
        | "method_definition" | "generator_function_declaration" => Some("Function"),
        "class_declaration" | "class" => Some("Class"),
        "interface_declaration" => Some("Interface"),
        "type_alias_declaration" | "type_specifier" => Some("TypeAlias"),
        "enum_declaration" => Some("Enum"),
        _ => None,
    }
}

fn go_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "method_declaration" => Some("Function"),
        "type_declaration" | "type_spec" => Some("TypeDeclaration"),
        _ => None,
    }
}

fn python_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("Function"),
        "class_definition" => Some("Class"),
        "decorated_definition" => Some("Decorated"),
        _ => None,
    }
}

fn ruby_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "method" | "singleton_method" => Some("Method"),
        "class" | "module" => Some("ClassOrModule"),
        _ => None,
    }
}

fn php_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" | "method_declaration" => Some("Function"),
        "class_declaration" | "interface_declaration" | "trait_declaration" => Some("Class"),
        _ => None,
    }
}

fn script_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" | "function" | "function_declaration" | "subroutine_declaration"
        | "proc_declaration" | "procedure_declaration" | "method" => Some("Function"),
        "class_definition" | "class" | "module" => Some("ClassOrModule"),
        _ => None,
    }
}

fn java_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "method_declaration" | "constructor_declaration" => Some("Method"),
        "class_declaration" | "interface_declaration" | "enum_declaration"
        | "annotation_type_declaration" | "record_declaration" => Some("Type"),
        _ => None,
    }
}

fn kotlin_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "primary_constructor" => Some("Function"),
        "class_declaration" | "object_declaration" | "interface_declaration" => Some("Type"),
        _ => None,
    }
}

fn csharp_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "method_declaration" | "constructor_declaration" => Some("Method"),
        "class_declaration" | "interface_declaration" | "struct_declaration"
        | "enum_declaration" | "record_declaration" => Some("Type"),
        _ => None,
    }
}

fn gdscript_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("Function"),
        "class_definition" | "class_name_statement" => Some("Class"),
        _ => None,
    }
}

fn glsl_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("Function"),
        "struct_specifier" => Some("Struct"),
        _ => None,
    }
}

fn swift_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "function_definition" | "init_declaration"
        | "deinit_declaration" => Some("Function"),
        "class_declaration" | "struct_declaration" | "enum_declaration"
        | "protocol_declaration" | "extension_declaration" => Some("Type"),
        _ => None,
    }
}

fn scala_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" | "function_declaration" => Some("Function"),
        "class_definition" | "object_definition" | "trait_definition" => Some("Type"),
        _ => None,
    }
}

fn haskell_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "function_definition" => Some("Function"),
        "data_type" | "newtype" | "type_alias" | "adt" => Some("Type"),
        "class_declaration" | "instance_declaration" => Some("ClassOrInstance"),
        "module" => Some("Module"),
        _ => None,
    }
}

fn elixir_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "call" | "anonymous_function" | "function" | "function_call" => Some("Function"),
        "module" | "defmodule" => Some("Module"),
        _ => None,
    }
}

fn ocaml_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "let_binding" | "let_expression" => Some("LetBinding"),
        "function" | "method_definition" => Some("Function"),
        "module_definition" => Some("Module"),
        "type_binding" | "type_definition" => Some("Type"),
        "class_definition" | "class_type_definition" => Some("Class"),
        _ => None,
    }
}

fn zig_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "fn_decl" => Some("Function"),
        "struct_declaration" | "enum_declaration" | "union_declaration" => Some("Type"),
        _ => None,
    }
}

fn r_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("Function"),
        "binary_operator" => None,
        _ => None,
    }
}

fn sql_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "create_table" | "create_view" | "create_index" => Some("DDL"),
        "select" | "insert" | "update" | "delete" | "merge" => Some("DML"),
        _ => None,
    }
}

fn html_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "element" | "script_element" | "style_element" => Some("Element"),
        _ => None,
    }
}

fn css_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "rule_set" | "at_rule" | "keyframes_statement" => Some("Rule"),
        _ => None,
    }
}

fn json_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "object" | "array" => Some("Container"),
        "pair" => Some("KeyValue"),
        _ => None,
    }
}

fn yaml_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "block_mapping_pair" | "flow_mapping" => Some("Mapping"),
        "block_sequence" | "flow_sequence" => Some("Sequence"),
        _ => None,
    }
}

fn toml_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "pair" | "table" | "inline_table" => Some("Entry"),
        "array" => Some("Array"),
        _ => None,
    }
}

fn markdown_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "section" | "heading" | "paragraph" | "list" | "code_block" => Some("Block"),
        _ => None,
    }
}

fn dockerfile_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "run_instruction" | "cmd_instruction" | "entrypoint_instruction"
        | "from_instruction" | "env_instruction" | "copy_instruction"
        | "add_instruction" | "arg_instruction" | "label_instruction"
        | "expose_instruction" | "volume_instruction" | "user_instruction"
        | "workdir_instruction" => Some("Instruction"),
        _ => None,
    }
}

fn erlang_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_clause" | "anonymous_function" => Some("Function"),
        "module_attribute" => Some("Module"),
        "record_declaration" => Some("Record"),
        _ => None,
    }
}

fn fsharp_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_or_value_defn" | "function_or_value_defn_binding"
        | "let_expression" | "member_defn" => Some("Function"),
        "module_defn" | "namespace" => Some("Module"),
        "type_defn" | "type_repr" => Some("Type"),
        _ => None,
    }
}

fn d_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "function_definition" | "delegate_declaration"
        | "template_declaration" => Some("Function"),
        "class_declaration" | "struct_declaration" | "interface_declaration"
        | "union_declaration" => Some("Type"),
        "module_declaration" => Some("Module"),
        _ => None,
    }
}

fn zust_kind(_kind: &str) -> Option<&'static str> {
    None // 当前不处理 zust,留给上层
}

fn extract_name(node: tree_sitter::Node<'_>, text: &str) -> Option<String> {
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Ok(s) = name_node.utf8_text(text.as_bytes()) {
            return Some(s.trim().to_string());
        }
    }
    // 兜底:找第一个 identifier 类型的 child
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            let k = child.kind();
            if k.ends_with("identifier")
                || k.ends_with("name")
                || k == "field_identifier"
                || k == "property_identifier"
                || k == "type_identifier"
            {
                if let Ok(s) = child.utf8_text(text.as_bytes()) {
                    return Some(s.trim().to_string());
                }
            }
        }
    }
    None
}

fn detect_public(
    node: tree_sitter::Node<'_>,
    text: &str,
    _kind: &str,
    language: Language,
) -> bool {
    let start = node.start_byte();
    // 用全 prefix(原来 80 字节 lookback 在节点首字节恰好落在 pub 之后时,
    // 会丢掉更靠前的修饰符;在多行文件里 `pub fn` 跨多行时也容易漏看)
    let prefix: &[u8] = if start == 0 { &[] } else { &text.as_bytes()[..start] };
    let prefix_str = String::from_utf8_lossy(prefix);
    let trimmed = prefix_str.trim_end();
    let tokens: Vec<&str> = trimmed
        .split(|c: char| c.is_whitespace() || c == '(' || c == '*' || c == '&')
        .filter(|s| !s.is_empty())
        .collect();
    let last = tokens.last().copied().unwrap_or("");
    match language {
        Language::Rust => {
            // tree-sitter-rust 把 `pub` / `pub(crate)` / `pub(super)` / `pub(self)` 解析为
            // `visibility_modifier` 子节点(在 children 而非 fields 里),通过 named_child
            // 直接搜类型拿权威结果,避免 prefix 字符串扫描带来的 false positive / negative。
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    if child.kind() == "visibility_modifier" {
                        let vis_text = child.utf8_text(text.as_bytes()).unwrap_or("");
                        if vis_text.starts_with("pub") {
                            return true;
                        }
                    }
                }
            }
            // 兜底:prefix 字符串检测,处理非常规 modifier
            if last == "pub" || last.starts_with("pub(") {
                return true;
            }
            // 倒数第二个 token 是 `pub`(应对 `pub fn` 这种 modifier 与 fn 分离)
            if tokens.len() >= 2 && tokens[tokens.len() - 2] == "pub" {
                return true;
            }
            false
        }
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            // export 是 sibling modifier,不是嵌套的;prefix 里**最近**的 export 才算数,
            // 否则上一行 `export function foo() {}` 的 export 会污染下一行的 `function bar()`。
            // 取最近 ~80 字符的 token 窗口。
            let recent_window: String = if trimmed.len() > 80 {
                trimmed[trimmed.len() - 80..].to_string()
            } else {
                trimmed.to_string()
            };
            let recent: Vec<&str> = recent_window
                .split(|c: char| c.is_whitespace() || c == '(' || c == '*' || c == '&')
                .filter(|s| !s.is_empty())
                .collect();
            recent.last().copied() == Some("export")
                || recent.last().copied() == Some("default")
                || recent.contains(&"export")
        }
        Language::Go => {
            // Go:首字母大写即 exported
            if let Some(name) = extract_name(node, text) {
                if let Some(first) = name.chars().next() {
                    return first.is_ascii_uppercase();
                }
            }
            true
        }
        Language::Java | Language::Kotlin | Language::CSharp => {
            // explicit `public`(同样只看最近窗口,避免上一个 class 的 `public` 串扰)
            let recent_window: String = if trimmed.len() > 80 {
                trimmed[trimmed.len() - 80..].to_string()
            } else {
                trimmed.to_string()
            };
            recent_window.contains("public ")
                || recent_window
                    .split(|c: char| c.is_whitespace() || c == '(' || c == '*' || c == '&')
                    .filter(|s| !s.is_empty())
                    .next_back()
                    == Some("public")
        }
        Language::C | Language::Cpp => {
            // 头文件或非 static 都算 public
            !prefix_str.contains("static")
        }
        Language::Python => {
            // 名字以 _ 开头即 private
            if let Some(name) = extract_name(node, text) {
                return !name.starts_with('_');
            }
            true
        }
        Language::Ruby => {
            // Ruby:method 名不带 `private_` 前缀即 public
            if let Some(name) = extract_name(node, text) {
                return !name.starts_with('_');
            }
            true
        }
        // Shell/Perl/Lua/Tcl:默认 public(没有显式 visibility 概念)
        Language::Shell | Language::Perl | Language::Lua | Language::Tcl => true,
        Language::Php | Language::GDScript | Language::Glsl | Language::Wgsl => {
            // 简化处理:看是否有 `private` / `protected` 关键字(也只看最近窗口)
            let recent_window: String = if trimmed.len() > 80 {
                trimmed[trimmed.len() - 80..].to_string()
            } else {
                trimmed.to_string()
            };
            !recent_window.contains("private") && !recent_window.contains("protected")
        }
        Language::Swift | Language::Scala | Language::Haskell | Language::Elixir
        | Language::OCaml | Language::Zig | Language::R | Language::SQL
        | Language::HTML | Language::CSS | Language::JSON | Language::YAML
        | Language::TOML | Language::Markdown | Language::Dockerfile
        | Language::Erlang | Language::FSharp | Language::D => {
            // 大部分声明默认 public;仅看是否有 `private` / `internal` 关键字
            !trimmed.contains("private") && !trimmed.contains("internal")
        }
        Language::Zust => true,
    }
}

fn unit_dynamic(u: &SyntaxUnit) -> Dynamic {
    let mut values: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    values.insert(smol_str::SmolStr::new("kind"), Dynamic::from(u.kind.as_str()));
    values.insert(
        smol_str::SmolStr::new("name"),
        match &u.name {
            Some(n) => Dynamic::from(n.as_str()),
            None => Dynamic::Null,
        },
    );
    values.insert(
        smol_str::SmolStr::new("start_line"),
        Dynamic::from(u.start_line as u64),
    );
    values.insert(
        smol_str::SmolStr::new("end_line"),
        Dynamic::from(u.end_line as u64),
    );
    values.insert(
        smol_str::SmolStr::new("is_public"),
        Dynamic::Bool(u.is_public),
    );
    values.insert(
        smol_str::SmolStr::new("parent"),
        match &u.parent {
            Some(p) => Dynamic::from(p.as_str()),
            None => Dynamic::Null,
        },
    );
    Dynamic::map(values)
}

fn dynamic_map(items: impl IntoIterator<Item = (&'static str, Dynamic)>) -> Dynamic {
    let mut values: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    for (k, v) in items {
        values.insert(smol_str::SmolStr::new(k), v);
    }
    Dynamic::map(values)
}

use tree_sitter::Parser;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// 临时目录辅助:测试结束时清理。
    /// path() 返回 canonicalize 后的路径,因为 macOS 的 /var、/tmp 是 /private/* 的 symlink,
    /// scan_repo 内部 canonicalize root,walkdir 用 canonical root 走;两边必须对齐。
    struct TempDir(PathBuf, PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "zust-scan-{}-{}",
                label,
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            Self(dir, canon)
        }
        /// 给 scan_repo 用的 canonical 绝对路径。
        fn path(&self) -> &Path {
            &self.1
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, body).unwrap();
    }

    // ---- supported_languages ----

    #[test]
    fn supported_languages_includes_core_set() {
        let langs = supported_languages();
        // 数量回归:目前 41 种(commit 时的快照)
        assert!(
            langs.len() >= 35,
            "supported_languages 数量异常: {}",
            langs.len()
        );
        for expected in [
            "Rust",
            "C",
            "Cpp",
            "JavaScript",
            "TypeScript",
            "Go",
            "Python",
            "JSON",
            "YAML",
            "TOML",
            "Markdown",
            "Dockerfile",
        ] {
            assert!(
                langs.contains(&expected),
                "缺少语言: {expected}; have = {langs:?}"
            );
        }
        // Zust 自家语言必须存在(后续可能要给它加 parser)
        assert!(langs.contains(&"Zust"));
    }

    // ---- relative_path ----

    #[test]
    fn relative_path_strips_root_and_normalizes_separators() {
        let root = Path::new("/abs/proj");
        assert_eq!(relative_path(root, Path::new("/abs/proj/src/lib.rs")), "src/lib.rs");
        assert_eq!(relative_path(root, Path::new("/abs/proj/a/b/c.rs")), "a/b/c.rs");
        // 不在 root 之下时,fallback 到原路径
        assert_eq!(relative_path(root, Path::new("/other/x.rs")), "/other/x.rs");
    }

    // ---- is_excluded ----

    #[test]
    fn is_excluded_blocks_common_build_dirs() {
        let root = Path::new("/abs/proj");
        for excluded in [
            "node_modules/lodash/index.js",
            "target/debug/main.rs",
            ".git/HEAD",
            ".github/workflows/ci.yml",
            "vendor/lib/foo.c",
            "build/output.bin",
            "dist/static/main.js",
        ] {
            assert!(
                is_excluded(root, Path::new(excluded)),
                "应排除: {excluded}"
            );
        }
        // 顶层文件不受影响
        assert!(!is_excluded(root, Path::new("src/main.rs")));
        // 同名目录出现在 root 之上时,不排除(只比相对 segments)
        assert!(!is_excluded(
            Path::new("/abs/vendor"),
            Path::new("/abs/vendor/fd")
        ));
    }

    #[test]
    fn is_excluded_blocks_autotools_boilerplate_files() {
        let root = Path::new("/abs/proj");
        for file in [
            "configure",
            "configure.ac",
            "configure.in",
            "missing",
            "install-sh",
            "depcomp",
            "compile",
            "config.guess",
        ] {
            // 文件名在排除列表里(即使不在子目录)
            // is_excluded 用 path.file_name() 判断,所以给个完整路径
            let p = format!("/abs/proj/sub/{file}");
            assert!(
                is_excluded(root, Path::new(&p)),
                "应排除 autotools 样板: {file}"
            );
        }
        // 普通源文件不应被排除
        assert!(!is_excluded(root, Path::new("/abs/proj/src/lib.rs")));
    }

    // ---- language_for ----

    #[test]
    fn language_for_recognizes_common_extensions() {
        let cases = [
            ("foo.rs", Some(Language::Rust)),
            ("foo.py", Some(Language::Python)),
            ("foo.go", Some(Language::Go)),
            ("foo.ts", Some(Language::TypeScript)),
            ("foo.tsx", Some(Language::Tsx)),
            ("foo.js", Some(Language::JavaScript)),
            ("foo.cpp", Some(Language::Cpp)),
            ("foo.cc", Some(Language::Cpp)),
            ("foo.h", Some(Language::C)),
            ("foo.toml", Some(Language::TOML)),
            ("foo.yaml", Some(Language::YAML)),
            ("foo.yml", Some(Language::YAML)),
            ("foo.json", Some(Language::JSON)),
            ("foo.md", Some(Language::Markdown)),
            // 注意:.zs / .zust 走 Language::Zust,但 lang_entry 返回 None,
            // 所以 language_for 返回 None(待 zust 自家 parser 接入后会变 Some(Zust))
            ("foo.zs", None),
            // Dockerfile 文件名没有扩展,会被 `_` 分支拦下返回 None;
            // 想匹配 Dockerfile 字样必须用 `.dockerfile` 扩展或后续提供精确文件名匹配。
            ("Dockerfile", None),
            ("foo.dockerfile", Some(Language::Dockerfile)),
            ("random.txt", None),
            ("Makefile", None),
        ];
        for (name, expected) in cases {
            let actual = language_for(Path::new(name)).map(|(lang, _)| lang);
            assert_eq!(
                actual, expected,
                "language_for({name:?}): expected {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn language_for_case_insensitive_for_known_extensions() {
        // 大写扩展名也应该识别(R 文件尤其常见 *.R)
        assert_eq!(
            language_for(Path::new("foo.R")).map(|(l, _)| l),
            Some(Language::R)
        );
        assert_eq!(
            language_for(Path::new("FOO.YAML")).map(|(l, _)| l),
            Some(Language::YAML)
        );
    }

    #[test]
    fn language_for_extension_match_takes_priority_over_filename() {
        // Dockerfile 是精确文件名匹配;这里测普通 .dockerfile 扩展名也能识别
        let (lang, _) = language_for(Path::new("foo.dockerfile")).expect("dockerfile ext");
        assert_eq!(lang, Language::Dockerfile);
    }

    // ---- harvest_units / classify / walk ----

    fn harvest_from_source(source: &str, language: Language) -> Vec<SyntaxUnit> {
        let entry = lang_entry(language).expect("lang_entry 应当可用");
        let mut parser = Parser::new();
        parser.set_language(&entry.lang).expect("set_language");
        let tree = parser.parse(source, None).expect("parse");
        harvest_units(tree.root_node(), source, language)
    }

    #[test]
    fn harvest_rust_extracts_pub_and_private_items() {
        let src = r#"
            pub fn public_fn() {}
            fn private_fn() {}
            pub struct Foo {}
            pub enum Color { Red, Green }
            pub trait Greet { fn hi(&self); }
            impl Foo {
                pub fn bar(&self) {}
            }
            mod inner {
                pub mod nested {}
            }
        "#;
        let units = harvest_from_source(src, Language::Rust);
        let names: Vec<_> = units
            .iter()
            .filter_map(|u| u.name.clone())
            .collect();
        assert!(names.contains(&"public_fn".to_string()));
        assert!(names.contains(&"private_fn".to_string()));
        assert!(names.contains(&"Foo".to_string()));
        assert!(names.contains(&"Color".to_string()));
        assert!(names.contains(&"Greet".to_string()));
        assert!(names.contains(&"inner".to_string()));
        assert!(names.contains(&"nested".to_string()));

        // public 检测
        let public_fn = units.iter().find(|u| u.name.as_deref() == Some("public_fn")).unwrap();
        assert!(public_fn.is_public);
        let private_fn = units.iter().find(|u| u.name.as_deref() == Some("private_fn")).unwrap();
        assert!(!private_fn.is_public);

        // kind
        assert_eq!(
            units.iter().find(|u| u.name.as_deref() == Some("Foo")).unwrap().kind,
            "Struct"
        );
        assert_eq!(
            units.iter().find(|u| u.name.as_deref() == Some("Color")).unwrap().kind,
            "Enum"
        );
    }

    #[test]
    fn harvest_python_extracts_function_and_class() {
        let src = r#"
            def public_fn(x, y):
                return x + y

            def _private_fn():
                pass

            class Foo:
                def __init__(self):
                    self.x = 1

            @decorator
            def decorated():
                pass
        "#;
        let units = harvest_from_source(src, Language::Python);
        let funcs: Vec<_> = units
            .iter()
            .filter(|u| u.kind == "Function")
            .filter_map(|u| u.name.clone())
            .collect();
        assert!(funcs.contains(&"public_fn".to_string()));
        assert!(funcs.contains(&"_private_fn".to_string()));
        assert!(funcs.contains(&"decorated".to_string()));
        // _private_fn Python 下是 private
        let private = units.iter().find(|u| u.name.as_deref() == Some("_private_fn")).unwrap();
        assert!(!private.is_public);
    }

    #[test]
    fn harvest_javascript_extracts_class_and_function() {
        // 用单行 / 单函数做单元判定,避免 prefix 串扰。
        // 这里的关键断言是:每个 function 的 visibility 判定只看自己的 sibling modifier,
        // 不被前一个函数的 `export` 污染。
        let public_units = harvest_from_source(
            "export function publicFn() {}\nexport class Foo {}\n",
            Language::JavaScript,
        );
        let kinds: Vec<_> = public_units.iter().map(|u| u.kind.as_str()).collect();
        assert!(kinds.contains(&"Function"));
        assert!(kinds.contains(&"Class"));

        let public_fn = public_units
            .iter()
            .find(|u| u.name.as_deref() == Some("publicFn"))
            .expect("publicFn");
        assert!(public_fn.is_public, "export function 应识别为 public");

        let private_units = harvest_from_source(
            "function privateFn() {}\nfunction anotherPrivate() {}\n",
            Language::JavaScript,
        );
        let private_fn = private_units
            .iter()
            .find(|u| u.name.as_deref() == Some("privateFn"))
            .expect("privateFn");
        assert!(
            !private_fn.is_public,
            "无 export 前缀的 function 应识别为 private"
        );
        let another_private = private_units
            .iter()
            .find(|u| u.name.as_deref() == Some("anotherPrivate"))
            .expect("anotherPrivate");
        assert!(!another_private.is_public);
    }

    #[test]
    fn harvest_go_exports_capitalized_names() {
        let src = r#"
            package foo
            func Public() {}
            func private() {}
            type Bar struct{}
        "#;
        let units = harvest_from_source(src, Language::Go);
        let public = units.iter().find(|u| u.name.as_deref() == Some("Public")).unwrap();
        assert!(public.is_public, "Go: 首字母大写即 exported");
        let private = units.iter().find(|u| u.name.as_deref() == Some("private")).unwrap();
        assert!(!private.is_public);
    }

    #[test]
    fn harvest_records_span_line_numbers_one_indexed() {
        let src = "fn first() {}\nfn second() {}\nfn third() {}\n";
        let units = harvest_from_source(src, Language::Rust);
        assert!(units.len() >= 3);
        // 1-indexed 起始行
        assert_eq!(units[0].start_line, 1);
        // 第二个函数应当从第 2 行开始
        let second = units
            .iter()
            .find(|u| u.name.as_deref() == Some("second"))
            .expect("second");
        assert_eq!(second.start_line, 2);
    }

    #[test]
    fn harvest_handles_files_with_no_units() {
        let units = harvest_from_source("// only comment\n", Language::Rust);
        assert!(units.is_empty(), "纯注释文件不应抽出任何 unit");
    }

    // ---- extract_name / detect_public 单元 ----

    #[test]
    fn detect_public_handles_rust_pub_variants() {
        // pub / pub(crate) / pub(super) / pub(self) 都视为 public
        let src = "pub fn a() {}\npub(crate) fn b() {}\npub(super) fn c() {}\npub(self) fn d() {}\n";
        let units = harvest_from_source(src, Language::Rust);
        for name in ["a", "b", "c", "d"] {
            let u = units.iter().find(|u| u.name.as_deref() == Some(name)).unwrap_or_else(|| panic!("miss {name}"));
            assert!(u.is_public, "{name} 应识别为 public");
        }
    }

    #[test]
    fn detect_public_marks_python_underscore_as_private() {
        let src = "def public_fn():\n    pass\ndef _internal():\n    pass\ndef __dunder__():\n    pass\n";
        let units = harvest_from_source(src, Language::Python);
        let public_fn = units.iter().find(|u| u.name.as_deref() == Some("public_fn")).unwrap();
        assert!(public_fn.is_public);
        let internal = units.iter().find(|u| u.name.as_deref() == Some("_internal")).unwrap();
        assert!(!internal.is_public);
    }

    // ---- scan_repo end-to-end ----

    #[test]
    fn scan_repo_returns_dynamic_with_files_and_language_stats() {
        let tmp = TempDir::new("e2e");
        write(
            tmp.path(),
            "src/lib.rs",
            "pub fn hello() -> i32 { 42 }\nfn private() {}\n",
        );
        write(
            tmp.path(),
            "src/main.rs",
            "fn main() { hello(); }\npub struct App {}\n",
        );
        write(tmp.path(), "README.md", "# Title\n\nSome content.\n");
        write(tmp.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
        // 应当被排除
        write(tmp.path(), "target/debug/foo.rs", "fn x() {}\n");
        write(tmp.path(), "node_modules/lib/index.js", "function f() {}\n");

        let result = scan_repo(tmp.path(), "demo-proj", "run-1").expect("scan_repo");

        // 顶层字段
        assert_eq!(result.get_dynamic("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            result.get_dynamic("project_id").map(|v| v.as_str().to_string()),
            Some("demo-proj".to_string())
        );
        assert_eq!(
            result.get_dynamic("run_id").map(|v| v.as_str().to_string()),
            Some("run-1".to_string())
        );
        assert_eq!(
            result.get_dynamic("total_files").and_then(|v| v.as_int()),
            Some(4), // 两个 .rs + README.md + Cargo.toml
            "target/ 和 node_modules/ 应被排除"
        );

        // files map
        let files = result.get_dynamic("files").expect("files map");
        assert!(files.contains("src/lib.rs"));
        assert!(files.contains("src/main.rs"));
        assert!(files.contains("README.md"));
        assert!(files.contains("Cargo.toml"));
        assert!(!files.contains("target/debug/foo.rs"));

        // 单文件 entry
        let lib = files.get_dynamic("src/lib.rs").expect("lib.rs");
        assert_eq!(
            lib.get_dynamic("language").map(|v| v.as_str().to_string()),
            Some("Rust".to_string())
        );
        let units = lib.get_dynamic("units").expect("units list");
        assert!(units.len() >= 2, "应当抽出 hello + private");

        // language_files 统计
        let lang_files = result.get_dynamic("language_files").expect("lang_files");
        let rust_count = (0..lang_files.len())
            .map(|i| lang_files.get_idx(i).unwrap())
            .find(|e| e.get_dynamic("language").map(|v| v.as_str().to_string()) == Some("Rust".to_string()))
            .expect("Rust 统计");
        assert_eq!(
            rust_count.get_dynamic("count").and_then(|v| v.as_int()),
            Some(2)
        );
    }

    #[test]
    fn scan_repo_continues_when_a_file_fails_to_parse() {
        // binary 文件不是 UTF-8 文本,会被 from_utf8_lossy 替换为乱码,
        // 但 parse 不应 panic。parser_failed_files 计数应当 > 0。
        let tmp = TempDir::new("bin");
        write(tmp.path(), "a.rs", "pub fn ok() {}\n");
        // 写入非法 UTF-8 字节序列
        fs::write(tmp.path().join("bad.rs"), [0xff, 0xfe, 0xfd, b'\n']).unwrap();
        write(tmp.path(), "c.py", "def also_ok():\n    pass\n");

        let result = scan_repo(tmp.path(), "p", "r").expect("scan_repo");
        // parser_failed_files >= 1 (bad.rs 的解析树会有错误但 Parser.parse 仍返回 Some)
        // 这里我们只检查 ok / total_files 仍然是合理的(不应 panic / 提前 return)
        assert_eq!(result.get_dynamic("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            result.get_dynamic("total_files").and_then(|v| v.as_int()),
            Some(3)
        );
        // a.rs 和 c.py 仍应当被列在 files 里
        let files = result.get_dynamic("files").expect("files");
        assert!(files.contains("a.rs"));
        assert!(files.contains("c.py"));
    }

    #[test]
    fn scan_repo_handles_empty_directory() {
        let tmp = TempDir::new("empty");
        let result = scan_repo(tmp.path(), "p", "r").expect("scan_repo");
        assert_eq!(result.get_dynamic("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            result.get_dynamic("total_files").and_then(|v| v.as_int()),
            Some(0)
        );
    }

    #[test]
    fn scan_repo_returns_supported_languages_from_main_api() {
        // 通过 build_context / register_natives 间接验证,这里直接看 supported_languages 与
        // scan_repo 的 language_files 中的 language 标签是否全部在 supported_languages 集合内
        let tmp = TempDir::new("langset");
        write(tmp.path(), "a.rs", "fn x(){}\n");
        write(tmp.path(), "b.py", "def y():\n    pass\n");
        let result = scan_repo(tmp.path(), "p", "r").expect("scan_repo");
        let lang_files = result.get_dynamic("language_files").expect("lang_files");
        let supported = supported_languages();
        for idx in 0..lang_files.len() {
            let entry = lang_files.get_idx(idx).unwrap();
            let lang = entry
                .get_dynamic("language")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default();
            assert!(
                supported.contains(&lang.as_str()),
                "scan_repo 报告了未在 supported_languages 列表中的语言: {lang}"
            );
        }
    }
}
