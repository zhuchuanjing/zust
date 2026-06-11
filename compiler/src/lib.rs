pub mod infer;
mod symbol;
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Parser, Pattern, PatternKind, Span, Stmt, StmtKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
pub use symbol::{Symbol, SymbolTable, eval_const_int_type, substitute_type};

#[derive(Clone)]
enum FnInferRet {
    Pending(Option<Type>),
    Done(Type),
}

#[derive(Clone)]
pub enum ListElemState {
    Unknown,
    Known(Type),
    Mixed,
}

#[derive(Clone)]
pub struct Compiler {
    pub symbols: SymbolTable,
    pub frames: Vec<usize>,
    pub tys: Vec<Type>,
    pub consts: Vec<Dynamic>,
    names: Vec<SmolStr>,
    list_elem_states: Vec<Option<ListElemState>>,
    arg_counts: Vec<usize>,
    fns: BTreeMap<u32, Vec<(Vec<Type>, Vec<Type>, FnInferRet)>>,
    local_type_hints: BTreeMap<u32, Vec<(Vec<Type>, Vec<Type>, Vec<Option<Type>>)>>,
    infer_stack: Vec<(u32, Vec<Type>, Vec<Type>)>,
    importing_paths: BTreeSet<PathBuf>,
}

fn impl_target_name(target: &Type) -> anyhow::Result<SmolStr> {
    match target {
        Type::Ident { name, .. } => Ok(name.clone()),
        _ => anyhow::bail!("impl 目标类型暂不支持: {:?}", target),
    }
}

#[cfg(test)]
mod tests {
    use super::{Compiler, Symbol};
    use dynamic::Type;

    #[test]
    fn inferred_function_return_type_is_written_back_to_symbol() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_infer_return",
            br#"
            pub fn is_alive() {
                true
            }

            pub fn can_act() {
                is_alive() && true && is_alive()
            }
            "#
            .to_vec(),
        )?;

        let is_alive = compiler.symbols.get_id("compiler_infer_return::is_alive")?;
        assert_eq!(compiler.infer_fn(is_alive, &[])?, Type::Bool);

        let (_, symbol) = compiler.symbols.get_symbol(is_alive)?;
        let Symbol::Fn { ty: Type::Fn { ret, .. }, .. } = symbol else {
            panic!("is_alive should be a function symbol");
        };
        assert_eq!(ret.as_ref(), &Type::Bool);

        let can_act = compiler.symbols.get_id("compiler_infer_return::can_act")?;
        assert_eq!(compiler.infer_fn(can_act, &[])?, Type::Bool);
        Ok(())
    }

    #[test]
    fn top_level_const_composite_resolves_const_idents() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_const_table",
            br#"
            pub const GEM_ATK = "atk";
            pub const GEM_DEF = "def";
            pub const GEM_TABLE = [
                { key: GEM_ATK, score: 3i32 },
                { key: GEM_DEF, score: 1i32 },
            ];
            "#
            .to_vec(),
        )?;

        let table = compiler.symbols.get_id("compiler_const_table::GEM_TABLE")?;
        let (_, symbol) = compiler.symbols.get_symbol(table)?;
        let Symbol::Const { value, .. } = symbol else {
            panic!("GEM_TABLE should be a const symbol");
        };

        let first = value.get_idx(0).expect("first table row");
        assert_eq!(first.get_dynamic("key").expect("key").as_str(), "atk");
        assert_eq!(first.get_dynamic("score").expect("score").as_int(), Some(3));
        Ok(())
    }

    #[test]
    fn const_unary_neg_handles_min_integer_literal() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_const_min_int",
            br#"
            pub const MIN_I32: i32 = -2147483648i32;
            "#
            .to_vec(),
        )?;

        let id = compiler.symbols.get_id("compiler_const_min_int::MIN_I32")?;
        let (_, symbol) = compiler.symbols.get_symbol(id)?;
        let Symbol::Const { value, .. } = symbol else {
            panic!("MIN_I32 should be a const symbol");
        };
        assert_eq!(value.as_int(), Some(i32::MIN as i64));
        Ok(())
    }

    #[test]
    fn return_check_resolves_function_args_before_body_compile() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_return_check_args",
            br#"
            pub fn no_value_return(flag: bool) {
                if flag {
                    return;
                }
            }

            pub fn tail_if(flag: bool) {
                if flag {
                    1
                } else {
                    2
                }
            }

            pub fn loop_index(low: i64, high: i64) {
                let total = 0i64;
                for i in low..high {
                    total += i;
                }
                total
            }

            pub fn closure_capture() {
                let base = 10i32;
                let add_base = |value: i32| {
                    value + base
                };
                add_base(1i32)
            }

            pub fn destructured_names() {
                let (left, right) = (3i32, 4i32);
                let [first, second] = [5i32, 6i32];
                let _ = first;
                left + right + second
            }
            "#
            .to_vec(),
        )?;

        let no_value_return = compiler.symbols.get_id("compiler_return_check_args::no_value_return")?;
        assert_eq!(compiler.infer_fn(no_value_return, &[Type::Bool])?, Type::Void);

        let tail_if = compiler.symbols.get_id("compiler_return_check_args::tail_if")?;
        assert_eq!(compiler.infer_fn(tail_if, &[Type::Bool])?, Type::I32);

        let loop_index = compiler.symbols.get_id("compiler_return_check_args::loop_index")?;
        assert_eq!(compiler.infer_fn(loop_index, &[Type::I64, Type::I64])?, Type::I64);

        Ok(())
    }

    #[test]
    fn return_check_infers_raw_assoc_calls_before_body_compile() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_return_check_assoc",
            br#"
            pub struct Box<N> {
                data: [u32; N],
            }

            impl Box<N> {
                pub fn make() {
                    Box<N>{ data: [0u32; N] }
                }

                pub fn ok(self: Box<N>) {
                    true
                }
            }

            pub fn main() {
                let item = Box<2>::make();
                if item.ok() {
                    1i32
                } else {
                    0i32
                }
            }
            "#
            .to_vec(),
        )?;

        let main_id = compiler.symbols.get_id("compiler_return_check_assoc::main")?;
        assert_eq!(compiler.infer_fn(main_id, &[])?, Type::I32);
        Ok(())
    }

    #[test]
    fn forward_function_call_in_bool_condition_infers_callee_first() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_forward_bool",
            br#"
            pub fn can_start() {
                if is_ready() {
                    return true;
                }
                false
            }

            pub fn is_ready() {
                true
            }
            "#
            .to_vec(),
        )?;

        let can_start = compiler.symbols.get_id("compiler_forward_bool::can_start")?;
        assert_eq!(compiler.infer_fn(can_start, &[])?, Type::Bool);

        let is_ready = compiler.symbols.get_id("compiler_forward_bool::is_ready")?;
        assert_eq!(compiler.infer_fn(is_ready, &[])?, Type::Bool);
        Ok(())
    }

    #[test]
    fn inferred_return_cache_keeps_pending_separate_from_any() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_pending_any",
            br#"
            pub fn dynamic_value(value) {
                value
            }

            pub fn bool_value() {
                true
            }
            "#
            .to_vec(),
        )?;

        let dynamic_value = compiler.symbols.get_id("compiler_pending_any::dynamic_value")?;
        assert_eq!(compiler.infer_fn(dynamic_value, &[Type::Any])?, Type::Any);

        let bool_value = compiler.symbols.get_id("compiler_pending_any::bool_value")?;
        assert_eq!(compiler.infer_fn(bool_value, &[])?, Type::Bool);
        Ok(())
    }

    #[test]
    fn recursive_function_uses_inferred_return_seed() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_recursive_return",
            br#"
            pub fn factorial(n: i64) {
                if n <= 1 {
                    return 1;
                }
                n * factorial(n - 1)
            }

            pub fn factorial_reversed(n: i64) {
                if n > 1 {
                    return n * factorial_reversed(n - 1);
                }
                1
            }
            "#
            .to_vec(),
        )?;

        let factorial = compiler.symbols.get_id("compiler_recursive_return::factorial")?;
        assert_eq!(compiler.infer_fn(factorial, &[Type::I64])?, Type::I64);

        let factorial_reversed = compiler.symbols.get_id("compiler_recursive_return::factorial_reversed")?;
        assert_eq!(compiler.infer_fn(factorial_reversed, &[Type::I64])?, Type::I64);
        Ok(())
    }

    #[test]
    fn generic_function_infers_type_param_from_arg() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_generic_identity",
            br#"
            pub fn identity<T>(value: T) {
                value
            }
            "#
            .to_vec(),
        )?;

        let identity = compiler.symbols.get_id("compiler_generic_identity::identity")?;
        assert_eq!(compiler.infer_fn(identity, &[Type::I64])?, Type::I64);
        assert_eq!(compiler.infer_fn(identity, &[Type::Bool])?, Type::Bool);
        Ok(())
    }

    #[test]
    fn generic_function_uses_explicit_const_param() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_generic_const",
            br#"
            pub fn value<N>() {
                N
            }
            "#
            .to_vec(),
        )?;

        let value = compiler.symbols.get_id("compiler_generic_const::value")?;
        assert_eq!(compiler.infer_fn_with_params(value, &[], &[Type::ConstInt(7)])?, Type::I32);
        Ok(())
    }

    #[test]
    fn generic_function_infers_const_param_from_array_len() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_generic_array_len",
            br#"
            pub fn len<N>(items: [i32; N]) {
                N
            }
            "#
            .to_vec(),
        )?;

        let len = compiler.symbols.get_id("compiler_generic_array_len::len")?;
        assert_eq!(compiler.infer_fn(len, &[Type::Array(std::rc::Rc::new(Type::I32), 3)])?, Type::I32);
        Ok(())
    }

    #[test]
    fn generic_function_reports_uninferred_param() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_generic_uninferred",
            br#"
            pub fn value<T>() {
                1i32
            }
            "#
            .to_vec(),
        )?;

        let value = compiler.symbols.get_id("compiler_generic_uninferred::value")?;
        let err = compiler.infer_fn(value, &[]).expect_err("generic parameter should not be inferred");
        assert!(format!("{err:#}").contains("无法从实参类型推断函数范型参数"));
        Ok(())
    }

    #[test]
    fn assignment_target_type_keeps_dynamic_index_sum_static() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_dynamic_index_sum",
            br#"
            pub fn sum_list(n: i64) {
                let l = [];
                for i in 0..n {
                    l.push(i);
                }
                let sum = 0i64;
                for i in 0..n {
                    sum = sum + l[i];
                }
                sum
            }
            "#
            .to_vec(),
        )?;

        let sum_list = compiler.symbols.get_id("compiler_dynamic_index_sum::sum_list")?;
        assert_eq!(compiler.infer_fn(sum_list, &[Type::I64])?, Type::I64);
        Ok(())
    }

    #[test]
    fn list_literal_infers_element_type() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        compiler.import_code(
            "compiler_list_elem_type",
            br#"
            pub fn pushed_empty() {
                let items = [];
                items.push(1i64);
                items[0]
            }

            pub fn ints() {
                [1i64, 2i64]
            }

            pub fn mixed_then_int() {
                let items = [];
                items.push(1i64);
                items.push("aaa");
                items.push(2i64);
                items[0]
            }

            "#
            .to_vec(),
        )?;

        let pushed_empty = compiler.symbols.get_id("compiler_list_elem_type::pushed_empty")?;
        assert_eq!(compiler.infer_fn(pushed_empty, &[])?, Type::I64);
        let hints = compiler.inferred_local_type_hints(pushed_empty, &[], &[]);
        assert_eq!(hints.first().cloned().flatten(), Some(Type::List(std::rc::Rc::new(Type::I64))));

        let ints = compiler.symbols.get_id("compiler_list_elem_type::ints")?;
        assert_eq!(compiler.infer_fn(ints, &[])?, Type::Any);

        let mixed_then_int = compiler.symbols.get_id("compiler_list_elem_type::mixed_then_int")?;
        assert_eq!(compiler.infer_fn(mixed_then_int, &[])?, Type::Any);
        let hints = compiler.inferred_local_type_hints(mixed_then_int, &[], &[]);
        assert_eq!(hints.first().cloned().flatten(), None);
        Ok(())
    }

    #[test]
    fn return_map_and_struct_is_type_error() -> anyhow::Result<()> {
        let mut compiler = Compiler::new();
        let err = match compiler.import_code(
            "compiler_return_map_struct",
            br#"
            struct S {
                hp: i32,
            }

            pub fn make_s_or_error(flag: i32) {
                if flag == 0 {
                    return { error: "bad" };
                }
                S{hp: 123}
            }
            "#
            .to_vec(),
        ) {
            Ok(_) => panic!("expected mismatched return types to fail"),
            Err(err) => err,
        };

        assert!(format!("{err:#}").contains("返回类型不一致"));
        Ok(())
    }
}

fn has_unresolved_generic_param(ty: &Type) -> bool {
    match ty {
        Type::Ident { name, params } => {
            if params.is_empty() {
                name.chars().next().map(|ch| ch.is_ascii_uppercase()).unwrap_or(false)
            } else {
                params.iter().any(has_unresolved_generic_param)
            }
        }
        Type::Struct { params, fields } => params.iter().any(has_unresolved_generic_param) || fields.iter().any(|(_, ty)| has_unresolved_generic_param(ty)),
        Type::Tuple(items) => items.iter().any(has_unresolved_generic_param),
        Type::List(elem) | Type::Vec(elem, _) | Type::Array(elem, _) => has_unresolved_generic_param(elem),
        Type::ArrayParam(elem, len) => has_unresolved_generic_param(elem) || has_unresolved_generic_param(len),
        Type::Fn { tys, ret } => tys.iter().any(has_unresolved_generic_param) || has_unresolved_generic_param(ret),
        Type::Symbol { params, .. } => params.iter().any(has_unresolved_generic_param),
        Type::ConstBinary { left, right, .. } => has_unresolved_generic_param(left) || has_unresolved_generic_param(right),
        _ => false,
    }
}

fn is_top_level_import_expr(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Call { obj, .. } if matches!(&obj.kind, ExprKind::Ident(name) if name.as_str() == "import")
    )
}

fn string_value(expr: &Expr) -> Option<&str> {
    if let ExprKind::Value(Dynamic::String(value)) = &expr.kind { Some(value.as_str()) } else { None }
}

fn import_decl(stmt: &Stmt) -> Option<(SmolStr, SmolStr)> {
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
        [module, path] => Some((string_value(module)?.into(), string_value(path)?.into())),
        [module] => match &module.kind {
            ExprKind::Value(Dynamic::String(value)) => Some((value.clone(), format!("{value}.zs").into())),
            ExprKind::Ident(value) => Some((value.clone(), format!("{value}.zs").into())),
            _ => None,
        },
        _ => None,
    }
}

fn generic_arg_for_name<'a>(name: &str, params: &'a [Type], args: &'a [Type]) -> Option<&'a Type> {
    params.iter().position(|param| matches!(param, Type::Ident { name: param_name, params } if params.is_empty() && param_name == name)).and_then(|idx| args.get(idx))
}

pub fn infer_generic_args_from_types(generic_params: &[Type], decl_tys: &[Type], arg_tys: &[Type]) -> Vec<Type> {
    if generic_params.is_empty() {
        return Vec::new();
    }
    let mut inferred = vec![None; generic_params.len()];
    for (decl, actual) in decl_tys.iter().zip(arg_tys.iter()) {
        infer_generic_arg_from_type(generic_params, decl, actual, &mut inferred);
    }
    if inferred.iter().all(|item| item.is_some()) {
        return inferred.into_iter().map(Option::unwrap).collect();
    }
    if let Some(Type::Struct { params, .. }) = arg_tys.iter().find(|ty| matches!(ty, Type::Struct { params, .. } if params.len() == generic_params.len())) {
        return params.clone();
    }
    for (decl, actual) in decl_tys.iter().zip(arg_tys.iter()) {
        if let (Type::Ident { params: decl_params, .. }, Type::Ident { params: actual_params, .. }) = (decl, actual)
            && decl_params.len() == actual_params.len()
            && decl_params.iter().any(|param| generic_params.contains(param))
        {
            return actual_params.clone();
        }
    }
    Vec::new()
}

pub fn resolve_generic_args_from_types(generic_params: &[Type], decl_tys: &[Type], arg_tys: &[Type], explicit_args: &[Type]) -> Result<Vec<Type>> {
    if generic_params.is_empty() {
        if explicit_args.is_empty() {
            return Ok(Vec::new());
        }
        return Err(anyhow!("函数不接受范型参数，但传入了 {}", explicit_args.len()));
    }
    if !explicit_args.is_empty() {
        if explicit_args.len() == generic_params.len() {
            return Ok(explicit_args.to_vec());
        }
        return Err(anyhow!("函数范型参数数量不匹配，期望 {} 个，实际 {} 个", generic_params.len(), explicit_args.len()));
    }

    let inferred = infer_generic_args_from_types(generic_params, decl_tys, arg_tys);
    if inferred.len() == generic_params.len() {
        Ok(inferred)
    } else if generic_params.len() == 1
        && let Some(Type::List(elem) | Type::Vec(elem, _) | Type::Array(elem, _)) = arg_tys.first()
    {
        Ok(vec![elem.as_ref().clone()])
    } else {
        Err(anyhow!("无法从实参类型推断函数范型参数 {:?}", generic_params))
    }
}

fn infer_generic_arg_from_type(generic_params: &[Type], decl: &Type, actual: &Type, inferred: &mut [Option<Type>]) {
    if let Some(idx) = generic_params.iter().position(|param| param == decl) {
        inferred[idx] = Some(actual.clone());
        return;
    }

    match (decl, actual) {
        (Type::List(decl_elem), Type::List(actual_elem)) => {
            infer_generic_arg_from_type(generic_params, decl_elem, actual_elem, inferred);
        }
        (Type::Vec(decl_elem, decl_len), Type::Vec(actual_elem, actual_len)) | (Type::Array(decl_elem, decl_len), Type::Array(actual_elem, actual_len)) => {
            infer_generic_arg_from_type(generic_params, decl_elem, actual_elem, inferred);
            infer_generic_arg_from_type(generic_params, &Type::ConstInt(*decl_len as i64), &Type::ConstInt(*actual_len as i64), inferred);
        }
        (Type::ArrayParam(decl_elem, decl_len), Type::Array(actual_elem, actual_len)) => {
            infer_generic_arg_from_type(generic_params, decl_elem, actual_elem, inferred);
            infer_generic_arg_from_type(generic_params, decl_len, &Type::ConstInt(*actual_len as i64), inferred);
        }
        (Type::Ident { params: decl_params, .. }, Type::Ident { params: actual_params, .. })
        | (Type::Ident { params: decl_params, .. }, Type::Symbol { params: actual_params, .. })
        | (Type::Symbol { params: decl_params, .. }, Type::Symbol { params: actual_params, .. })
        | (Type::Symbol { params: decl_params, .. }, Type::Ident { params: actual_params, .. })
        | (Type::Struct { params: decl_params, .. }, Type::Struct { params: actual_params, .. }) => {
            for (decl, actual) in decl_params.iter().zip(actual_params.iter()) {
                infer_generic_arg_from_type(generic_params, decl, actual, inferred);
            }
        }
        _ => {}
    }
}

fn substitute_pattern(pattern: &Pattern, params: &[Type], args: &[Type]) -> Pattern {
    let kind = match &pattern.kind {
        PatternKind::Ident { name, ty } => PatternKind::Ident { name: name.clone(), ty: substitute_type(ty, params, args) },
        PatternKind::Var { idx, ty } => PatternKind::Var { idx: *idx, ty: substitute_type(ty, params, args) },
        PatternKind::Tuple(items) => PatternKind::Tuple(items.iter().map(|item| substitute_pattern(item, params, args)).collect()),
        PatternKind::List { elems, has_rest } => PatternKind::List { elems: elems.iter().map(|item| substitute_pattern(item, params, args)).collect(), has_rest: *has_rest },
        other => other.clone(),
    };
    Pattern { kind, span: pattern.span }
}

fn substitute_expr(expr: &Expr, params: &[Type], args: &[Type]) -> Expr {
    let kind = match &expr.kind {
        ExprKind::Ident(name) => match generic_arg_for_name(name, params, args) {
            Some(Type::ConstInt(value)) => ExprKind::Value(Dynamic::I32(*value as i32)),
            Some(ty) => eval_const_int_type(ty).map(|value| ExprKind::Value(Dynamic::I32(value as i32))).unwrap_or_else(|| expr.kind.clone()),
            _ => expr.kind.clone(),
        },
        ExprKind::Typed { value, ty } => ExprKind::Typed { value: Box::new(substitute_expr(value, params, args)), ty: substitute_type(ty, params, args) },
        ExprKind::Unary { op, value } => ExprKind::Unary { op: op.clone(), value: Box::new(substitute_expr(value, params, args)) },
        ExprKind::Binary { left, op, right } => ExprKind::Binary { left: Box::new(substitute_expr(left, params, args)), op: op.clone(), right: Box::new(substitute_expr(right, params, args)) },
        ExprKind::Generic { obj, params: nested } => ExprKind::Generic { obj: Box::new(substitute_expr(obj, params, args)), params: nested.iter().map(|param| substitute_type(param, params, args)).collect() },
        ExprKind::Assoc { ty, name } => ExprKind::Assoc { ty: substitute_type(ty, params, args), name: name.clone() },
        ExprKind::TypedMethod { obj, ty, name } => ExprKind::TypedMethod { obj: Box::new(substitute_expr(obj, params, args)), ty: substitute_type(ty, params, args), name: name.clone() },
        ExprKind::AssocId { id, params: nested } => ExprKind::AssocId { id: *id, params: nested.iter().map(|param| substitute_type(param, params, args)).collect() },
        ExprKind::Tuple(items) => ExprKind::Tuple(items.iter().map(|item| substitute_expr(item, params, args)).collect()),
        ExprKind::List(items) => ExprKind::List(items.iter().map(|item| substitute_expr(item, params, args)).collect()),
        ExprKind::Repeat { value, len } => ExprKind::Repeat { value: Box::new(substitute_expr(value, params, args)), len: substitute_type(len, params, args) },
        ExprKind::Dict(items) => ExprKind::Dict(items.iter().map(|(name, value)| (name.clone(), substitute_expr(value, params, args))).collect()),
        ExprKind::Range { start, stop, inclusive } => ExprKind::Range { start: Box::new(substitute_expr(start, params, args)), stop: Box::new(substitute_expr(stop, params, args)), inclusive: *inclusive },
        ExprKind::Call { obj, params: call_params } => ExprKind::Call { obj: Box::new(substitute_expr(obj, params, args)), params: call_params.iter().map(|param| substitute_expr(param, params, args)).collect() },
        ExprKind::Stmt(stmt) => ExprKind::Stmt(Box::new(substitute_stmt(stmt, params, args))),
        ExprKind::Closure { args: closure_args, body } => {
            ExprKind::Closure { args: closure_args.iter().map(|(name, ty)| (name.clone(), substitute_type(ty, params, args))).collect(), body: Box::new(substitute_stmt(body, params, args)) }
        }
        _ => expr.kind.clone(),
    };
    Expr::new(kind, expr.span)
}

pub fn substitute_stmt(stmt: &Stmt, params: &[Type], args: &[Type]) -> Stmt {
    let kind = match &stmt.kind {
        StmtKind::Let { pat, value } => StmtKind::Let { pat: substitute_pattern(pat, params, args), value: Box::new(substitute_stmt(value, params, args)) },
        StmtKind::Expr(expr, close) => StmtKind::Expr(substitute_expr(expr, params, args), *close),
        StmtKind::Block(stmts) => StmtKind::Block(stmts.iter().map(|stmt| substitute_stmt(stmt, params, args)).collect()),
        StmtKind::Return(expr) => StmtKind::Return(expr.as_ref().map(|expr| substitute_expr(expr, params, args))),
        StmtKind::While { cond, body } => StmtKind::While { cond: substitute_expr(cond, params, args), body: Box::new(substitute_stmt(body, params, args)) },
        StmtKind::Loop(body) => StmtKind::Loop(Box::new(substitute_stmt(body, params, args))),
        StmtKind::For { pat, range, body } => StmtKind::For { pat: substitute_pattern(pat, params, args), range: substitute_expr(range, params, args), body: Box::new(substitute_stmt(body, params, args)) },
        StmtKind::Fn { name, generic_params, args: fn_args, body, is_pub } => StmtKind::Fn {
            name: name.clone(),
            generic_params: generic_params.iter().map(|param| substitute_type(param, params, args)).collect(),
            args: fn_args.iter().map(|(name, ty)| (name.clone(), substitute_type(ty, params, args))).collect(),
            body: Box::new(substitute_stmt(body, params, args)),
            is_pub: *is_pub,
        },
        StmtKind::Struct { name, def, is_pub } => StmtKind::Struct { name: name.clone(), def: substitute_type(def, params, args), is_pub: *is_pub },
        StmtKind::Impl { target, body } => StmtKind::Impl { target: substitute_type(target, params, args), body: Box::new(substitute_stmt(body, params, args)) },
        StmtKind::If { cond, then_body, else_body } => StmtKind::If {
            cond: substitute_expr(cond, params, args),
            then_body: Box::new(substitute_stmt(then_body, params, args)),
            else_body: else_body.as_ref().map(|body| Box::new(substitute_stmt(body, params, args))),
        },
        StmtKind::Static { name, ty, value, is_pub } => {
            StmtKind::Static { name: name.clone(), ty: substitute_type(ty, params, args), value: value.as_ref().map(|value| substitute_expr(value, params, args)), is_pub: *is_pub }
        }
        StmtKind::Const { name, ty, value, is_pub } => StmtKind::Const { name: name.clone(), ty: substitute_type(ty, params, args), value: substitute_expr(value, params, args), is_pub: *is_pub },
        other => other.clone(),
    };
    Stmt::new(kind, stmt.span)
}

#[derive(Debug, Clone, Default)]
pub struct Capture {
    pub names: Vec<(SmolStr, Type)>,
    pub vars: Vec<usize>,
}

impl Capture {
    pub fn new(names: Vec<(SmolStr, Type)>) -> Self {
        Self { names, vars: Vec::new() }
    }

    pub fn get(&mut self, name: &str) -> Option<usize> {
        if let Some(idx) = self.names.iter().position(|n| n.0 == name) {
            if let Some(pos) = self.vars.iter().position(|v| *v == idx) {
                Some(pos)
            } else {
                self.vars.push(idx);
                Some(self.vars.len() - 1)
            }
        } else {
            None
        }
    }

    pub fn get_type(&self, idx: u32) -> Option<Type> {
        self.names.get(idx as usize).map(|(_, ty)| ty.clone())
    }
}

use anyhow::{Context, Result, anyhow};
use smol_str::SmolStr;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{message}")]
pub struct SpannedCompilerError {
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CompilerDiagnostic {
    pub message: String,
    pub span: Span,
}

impl Compiler {
    pub fn clear(&mut self) {
        self.frames.clear();
        self.names.clear();
        self.tys.clear();
        self.list_elem_states.clear();
        self.arg_counts.clear();
    }

    pub fn take_local_state(&mut self) -> (Vec<usize>, Vec<SmolStr>, Vec<Type>, Vec<Option<ListElemState>>, Vec<usize>) {
        (std::mem::take(&mut self.frames), std::mem::take(&mut self.names), std::mem::take(&mut self.tys), std::mem::take(&mut self.list_elem_states), std::mem::take(&mut self.arg_counts))
    }

    pub fn restore_local_state(&mut self, state: (Vec<usize>, Vec<SmolStr>, Vec<Type>, Vec<Option<ListElemState>>, Vec<usize>)) {
        self.frames = state.0;
        self.names = state.1;
        self.tys = state.2;
        self.list_elem_states = state.3;
        self.arg_counts = state.4;
    }

    pub fn get_value(&self, expr: &Expr) -> Option<Dynamic> {
        match &expr.kind {
            ExprKind::Value(v) => Some(v.clone()),
            ExprKind::Const(idx) => self.consts.get(*idx).cloned(),
            _ => None,
        }
    }

    pub fn get_const(&mut self, value: Dynamic) -> usize {
        self.consts.iter().position(|c| c == &value).unwrap_or_else(|| {
            self.consts.push(value);
            self.consts.len() - 1
        })
    }

    fn normalize_self_assign(left: Expr, op: BinaryOp, right: Expr, span: Span, arg_count: usize) -> Expr {
        if let Some(idx) = left.var()
            && (idx as usize) < arg_count
        {
            let base_op = match op {
                BinaryOp::AddAssign => Some(BinaryOp::Add),
                BinaryOp::SubAssign => Some(BinaryOp::Sub),
                BinaryOp::MulAssign => Some(BinaryOp::Mul),
                BinaryOp::DivAssign => Some(BinaryOp::Div),
                BinaryOp::ModAssign => Some(BinaryOp::Mod),
                BinaryOp::ShlAssign => Some(BinaryOp::Shl),
                BinaryOp::ShrAssign => Some(BinaryOp::Shr),
                BinaryOp::BitAndAssign => Some(BinaryOp::BitAnd),
                BinaryOp::BitOrAssign => Some(BinaryOp::BitOr),
                BinaryOp::BitXorAssign => Some(BinaryOp::BitXor),
                _ => None,
            };
            if let Some(op) = base_op {
                let right = Expr::new(ExprKind::Binary { left: Box::new(left.clone()), op, right: Box::new(right) }, span);
                return Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Assign, right: Box::new(right) }, span);
            }
        }
        if op == BinaryOp::Assign
            && let Some(idx) = left.var()
            && idx as usize >= arg_count
            && let ExprKind::Binary { left: rhs_left, op: rhs_op, right: rhs_right } = &right.kind
            && rhs_left.var() == Some(idx)
        {
            let compound_op = match rhs_op {
                BinaryOp::Add => Some(BinaryOp::AddAssign),
                BinaryOp::Sub => Some(BinaryOp::SubAssign),
                BinaryOp::Mul => Some(BinaryOp::MulAssign),
                BinaryOp::Div => Some(BinaryOp::DivAssign),
                BinaryOp::Mod => Some(BinaryOp::ModAssign),
                BinaryOp::Shl => Some(BinaryOp::ShlAssign),
                BinaryOp::Shr => Some(BinaryOp::ShrAssign),
                BinaryOp::BitAnd => Some(BinaryOp::BitAndAssign),
                BinaryOp::BitOr => Some(BinaryOp::BitOrAssign),
                BinaryOp::BitXor => Some(BinaryOp::BitXorAssign),
                _ => None,
            };
            if let Some(op) = compound_op {
                return Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new((**rhs_right).clone()) }, span);
            }
        }
        Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new(right) }, span)
    }

    pub fn top(&self) -> usize {
        self.frames.last().copied().unwrap_or(0)
    }

    fn add_name(&mut self, name: SmolStr) -> u32 {
        self.names.push(name);
        (self.names.len() - self.top() - 1) as u32
    }

    fn list_elem_state_for_ty(ty: &Type) -> Option<ListElemState> {
        match ty {
            Type::List(elem) if elem.is_any() => Some(ListElemState::Unknown),
            Type::List(elem) => Some(ListElemState::Known(elem.as_ref().clone())),
            _ => None,
        }
    }

    pub(crate) fn list_elem_state(&self, idx: u32) -> Option<ListElemState> {
        self.list_elem_states.get(self.top() + idx as usize).cloned().flatten()
    }

    pub(crate) fn set_list_elem_state(&mut self, idx: u32, state: Option<ListElemState>) {
        let pos = idx as usize + self.top();
        if self.list_elem_states.len() <= pos {
            self.list_elem_states.resize(pos + 1, None);
        }
        self.list_elem_states[pos] = state;
    }

    fn add_ty(&mut self, ty: Type) -> u32 {
        self.list_elem_states.push(Self::list_elem_state_for_ty(&ty));
        self.tys.push(ty);
        (self.tys.len() - self.top() - 1) as u32
    }

    fn set_ty(&mut self, idx: u32, ty: Type) {
        let pos = idx as usize + self.top();
        if self.list_elem_states.len() <= pos {
            self.list_elem_states.resize(pos + 1, None);
        }
        self.list_elem_states[pos] = Self::list_elem_state_for_ty(&ty);
        if pos < self.tys.len() {
            self.tys[pos] = ty;
        } else if pos == self.tys.len() {
            self.tys.push(ty);
        } else {
            self.tys.resize(pos + 1, Type::Any);
            self.tys[pos] = ty;
        }
    }

    pub fn add_symbol(&mut self, name: &str, s: Symbol) -> u32 {
        self.symbols.add(name.into(), s)
    }

    pub fn new() -> Self {
        let symbols = SymbolTable::default();
        Self {
            symbols,
            tys: Vec::new(),
            names: Vec::new(),
            consts: Vec::with_capacity(10240),
            frames: Vec::new(),
            list_elem_states: Vec::new(),
            arg_counts: Vec::new(),
            fns: BTreeMap::new(),
            local_type_hints: BTreeMap::new(),
            infer_stack: Vec::new(),
            importing_paths: BTreeSet::new(),
        }
    }

    fn byte_to_line_col(src: &[u8], pos: usize) -> (usize, usize) {
        let mut line = 1;
        let mut col = 1;
        for &b in src.iter().take(pos.min(src.len())) {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn line_snippet(code: &[u8], span: Span) -> String {
        let pos = span.start.min(code.len());
        let line_start = code[..pos].iter().rposition(|&b| b == b'\n').map(|idx| idx + 1).unwrap_or(0);
        let line_end = code[pos..].iter().position(|&b| b == b'\n').map(|idx| pos + idx).unwrap_or(code.len());
        String::from_utf8_lossy(&code[line_start..line_end]).into_owned()
    }

    fn semantic_error(span: Span, message: impl Into<String>) -> anyhow::Error {
        SpannedCompilerError { message: message.into(), span }.into()
    }

    fn format_compile_error(code: &[u8], err: anyhow::Error) -> anyhow::Error {
        if let Some(err) = err.downcast_ref::<SpannedCompilerError>() {
            let pos = err.span.start.min(code.len());
            let (line, col) = Self::byte_to_line_col(code, pos);
            let snippet = Self::line_snippet(code, err.span);
            anyhow!("语义错误：第 {line} 行，第 {col} 列（字节偏移 {pos}）：{}\n{}", err.message, snippet)
        } else {
            err
        }
    }

    pub fn parse_code(code: Vec<u8>) -> Result<Vec<Stmt>> {
        let mut p = Parser::new(code.clone());
        let mut stmts = Vec::new();
        loop {
            match p.stmt(false) {
                Ok(stmt) => stmts.push(stmt),
                Err(e) => {
                    if p.is_eof() {
                        return Ok(stmts);
                    }
                    let pos = p.current_pos();
                    let (line, col) = Self::byte_to_line_col(&code, pos);
                    return Err(anyhow!("解析错误：第 {line} 行，第 {col} 列（字节偏移 {pos}）：{e:#}\n{}", p.error_stmt()));
                }
            }
        }
    }

    pub fn import_code(&mut self, name: &str, code: Vec<u8>) -> Result<Vec<u32>> {
        self.import_code_with_base_dir(name, code, None)
    }

    pub fn import_code_from_path(&mut self, name: &str, code: Vec<u8>, path: impl AsRef<Path>) -> Result<Vec<u32>> {
        self.import_code_with_base_dir(name, code, path.as_ref().parent())
    }

    pub fn import_file(&mut self, name: &str, path: impl AsRef<Path>) -> Result<Vec<u32>> {
        let path = path.as_ref();
        let canonical = std::fs::canonicalize(path).with_context(|| format!("failed to resolve import path {}", path.display()))?;
        if !self.importing_paths.insert(canonical.clone()) {
            return Ok(Vec::new());
        }
        let code = std::fs::read(&canonical).with_context(|| format!("failed to read import path {}", canonical.display()))?;
        let result = self.import_code_from_path(name, code, &canonical);
        self.importing_paths.remove(&canonical);
        result
    }

    fn import_code_with_base_dir(&mut self, name: &str, code: Vec<u8>, base_dir: Option<&Path>) -> Result<Vec<u32>> {
        let stmts = Self::parse_code(code.clone())?;
        log::debug!("func->{}", name);
        for s in stmts.iter() {
            log::debug!("{}", s);
        }
        self.resolve_imports(&stmts, base_dir).map_err(|err| Self::format_compile_error(&code, err))?;
        self.clear();
        self.compile(name.into(), stmts).map_err(|err| Self::format_compile_error(&code, err))
    }

    pub fn resolve_imports(&mut self, stmts: &[Stmt], base_dir: Option<&Path>) -> Result<()> {
        for stmt in stmts {
            let Some((module, path)) = import_decl(stmt) else {
                continue;
            };
            if !self.symbols.symbol(module.as_str()).is_empty() {
                continue;
            }
            let path = Path::new(path.as_str());
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(base_dir) = base_dir {
                base_dir.join(path)
            } else {
                std::env::current_dir()?.join(path)
            };
            self.import_file(module.as_str(), &resolved).with_context(|| format!("failed to import {module} from {}", resolved.display()))?;
        }
        Ok(())
    }

    pub fn check_code(name: &str, code: Vec<u8>) -> Vec<CompilerDiagnostic> {
        let mut parser = Parser::new(code.clone());
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

        let mut compiler = Self::new();
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

    pub fn get_field(&self, ty: &Type, name: &str) -> Result<(usize, Type)> {
        self.symbols.get_field(ty, name)
    }

    pub fn get_ident(&mut self, ident: &str, span: Span) -> Result<Expr> {
        for idx in (self.top()..self.names.len()).rev() {
            if self.names[idx].eq(ident) {
                return Ok(Expr::new(ExprKind::Var((idx - self.top()) as u32), span));
            }
        }
        let id = self.symbols.get_id(ident).map_err(|_| Self::semantic_error(span, format!("未找到标识符 {}", ident)))?;
        let s = self.symbols.get_symbol(id).map(|(_, v)| v.clone()).unwrap();
        if let Symbol::Const { value, ty, .. } = s {
            let c = self.get_const(value);
            return Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::Const(c), span)), ty }, span));
        } else if let Symbol::Static { value, ty, .. } = s
            && let Some(v) = value
        {
            let c = self.get_const(v);
            return Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::Const(c), span)), ty }, span));
        }
        Ok(Expr::new(ExprKind::Id(id, None), span))
    }

    fn field_access_expr(&mut self, left: Expr, idx: usize, ty: Type, key: &str, span: Span) -> Expr {
        if let Type::Symbol { id, .. } = ty {
            Expr::new(ExprKind::Id(id, Some(Box::new(left))), span)
        } else if ty.is_bool() && idx == usize::MAX {
            Expr::new(ExprKind::Value(Dynamic::Bool(false)), span)
        } else if ty.is_any() && idx == usize::MAX {
            let right = Expr::new(ExprKind::Const(self.get_const(Dynamic::String(key.into()))), span);
            Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(right) }, span)
        } else {
            Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(Expr::new(ExprKind::Value(Dynamic::U32(idx as u32)), span)) }, span)
        }
    }

    fn literal_field_access_expr(&mut self, left: Expr, key: &str, span: Span) -> Expr {
        let right = Expr::new(ExprKind::Const(self.get_const(Dynamic::String(key.into()))), span);
        Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(right) }, span)
    }

    fn type_field_access_expr(&mut self, left: Expr, key: &str, span: Span, prefer_dynamic_field: bool) -> Option<Expr> {
        let ty = self.infer_expr(&left).ok()?;
        if prefer_dynamic_field && ty.is_any() {
            return Some(self.literal_field_access_expr(left, key, span));
        }
        let (idx, field_ty) = self.get_field(&ty, key).ok()?;
        Some(self.field_access_expr(left, idx, field_ty, key, span))
    }

    fn global_method_access_expr(&self, left: Expr, method: &str, span: Span) -> Result<Option<Expr>> {
        let Ok(id) = self.symbols.get_id(method) else {
            return Ok(None);
        };
        if self.symbols.get_symbol(id)?.1.is_fn() { Ok(Some(Expr::new(ExprKind::Id(id, Some(Box::new(left))), span))) } else { Ok(None) }
    }

    fn method_call_obj_expr(&mut self, obj: &Expr, stmts: &mut Vec<Stmt>, cap: &mut Capture) -> Result<Option<Expr>> {
        if let ExprKind::TypedMethod { obj: left, ty, name } = &obj.kind {
            let left = self.eval(left, stmts, cap)?;
            let base_name = match ty {
                Type::Ident { name, .. } => name.clone(),
                Type::Symbol { id, .. } => self.symbols.get_symbol(*id)?.0.clone(),
                _ => return Err(Self::semantic_error(obj.span, format!("方法调用类型提示必须是类型: {:?}", ty))),
            };
            let method = format!("{}::{}", base_name, name);
            let id = self.symbols.get_id(&method).map_err(|_| Self::semantic_error(obj.span, format!("未找到类型方法 {}", method)))?;
            return Ok(Some(Expr::new(ExprKind::Id(id, Some(Box::new(left))), obj.span)));
        }

        let ExprKind::Binary { left, op: BinaryOp::Idx, right } = &obj.kind else {
            return Ok(None);
        };
        let Some(method) = self.get_value(right).and_then(|v| if v.is_str() { Some(v.as_str().to_string()) } else { None }) else {
            return Ok(None);
        };
        let left = self.eval(left, stmts, cap)?;
        if let Some(field) = self.type_field_access_expr(left.clone(), &method, obj.span, false) {
            return Ok(Some(field));
        }
        if let Some(method_fn) = self.global_method_access_expr(left.clone(), &method, obj.span)? {
            return Ok(Some(method_fn));
        }
        Ok(Some(self.literal_field_access_expr(left, &method, obj.span)))
    }

    pub fn compile_fn(&mut self, args: &[SmolStr], tys: &mut Vec<Type>, body: Stmt, cap: &mut Capture) -> Result<Vec<Stmt>> {
        let top = self.tys.len();
        self.frames.push(top);
        self.arg_counts.push(args.len());
        let result = (|| -> Result<Vec<Stmt>> {
            for (arg, ty) in args.iter().zip(tys.iter_mut()) {
                *ty = self.symbols.get_type(ty)?;
                self.add_name(arg.clone());
                self.add_ty(ty.clone());
            }
            if cap.names.is_empty() && tys.iter().all(|ty| !ty.is_any()) {
                let saved_state = (self.frames.clone(), self.names.clone(), self.tys.clone(), self.list_elem_states.clone(), self.arg_counts.clone());
                let result = self.check_return_type(&body);
                self.restore_local_state(saved_state);
                result?;
            }
            let mut compiled = Vec::new();
            self.compile_stmt(body, &mut compiled, cap)?;
            if !compiled.last_mut().map(|stmt| stmt.last_return()).unwrap_or(false) {
                compiled.push(Stmt::new(StmtKind::Return(None), Span::default()));
            }
            Ok(compiled)
        })();
        if let Some(top) = self.frames.pop() {
            self.tys.truncate(top);
            self.names.truncate(top);
            self.list_elem_states.truncate(top);
        }
        self.arg_counts.pop();
        result
    }

    pub fn compile(&mut self, mod_name: SmolStr, stmts: Vec<Stmt>) -> Result<Vec<u32>> {
        self.symbols.add_module(mod_name.clone());
        for stmt in stmts {
            match stmt.kind {
                StmtKind::Struct { name, def, is_pub } => {
                    self.symbols.add(name, Symbol::Struct(def, is_pub));
                }
                StmtKind::Static { name, ty, value, is_pub } => {
                    self.symbols.add(name, Symbol::Static { value: value.and_then(|v| v.value().ok()), ty, is_pub });
                }
                StmtKind::Const { name, ty, value, is_pub } => {
                    let value = self.const_expr_value(&value)?;
                    let ty = if ty.is_any() { value.get_type() } else { ty };
                    self.symbols.add(name, Symbol::Const { value, ty, is_pub });
                }
                StmtKind::Fn { name, generic_params, args, body, is_pub } => {
                    let (ty, args) = Type::from_args(args);
                    self.symbols.add(name, Symbol::Fn { ty, args, generic_params, cap: Capture::default(), body: Arc::new(*body), is_pub });
                }
                StmtKind::Impl { target, body } => {
                    let name = impl_target_name(&target)?;
                    let def_id = match self.symbols.get_id(&name) {
                        Ok(id) => id,
                        Err(_) if name.as_str() == "Vec" => self.symbols.add(name.clone(), Symbol::Struct(Type::Struct { params: Vec::new(), fields: Vec::new() }, true)),
                        Err(err) => return Err(err),
                    };
                    if let StmtKind::Block(fns) = body.kind {
                        for f in fns {
                            if let StmtKind::Fn { name: fn_name, generic_params: fn_generic_params, args, body, is_pub } = f.kind {
                                let (ty, args) = Type::from_args(args);
                                let mut generic_params = if has_unresolved_generic_param(&target) {
                                    match &target {
                                        Type::Ident { params, .. } => params.clone(),
                                        _ => Vec::new(),
                                    }
                                } else {
                                    Vec::new()
                                };
                                for param in fn_generic_params {
                                    if !generic_params.contains(&param) {
                                        generic_params.push(param);
                                    }
                                }
                                let fn_id = self.symbols.add(SmolStr::from(format!("{}::{}", name, fn_name)), Symbol::Fn { ty, args, generic_params, cap: Capture::default(), body: Arc::new(*body), is_pub });
                                if let Symbol::Struct(ty, _) = &mut self.symbols.symbols[def_id as usize] {
                                    ty.add_field(fn_name.into(), Type::Symbol { id: fn_id, params: Vec::new() })?;
                                }
                            } else {
                                log::debug!("impl 包含非函数语句 {:?}", f);
                            }
                        }
                    }
                }
                StmtKind::Expr(expr, _) if is_top_level_import_expr(&expr) => {}
                _ => {
                    log::debug!("未知的顶层语句 {:?}", stmt);
                }
            }
        }
        let mut fn_ids = Vec::new();
        for (name, id) in self.symbols.symbol(&mod_name) {
            log::debug!("compile symbol {:?}[{}]", name, id);
            if let Some((_, Symbol::Fn { ty, generic_params, .. })) = self.symbols.get_symbol(id).ok() {
                let resolved_ty = self.symbols.get_type(ty).unwrap_or_else(|_| ty.clone());
                if has_unresolved_generic_param(&resolved_ty) || !generic_params.is_empty() {
                    continue;
                }
            }
            if let Some(s) = self.symbols.get_symbol(id).ok().map(|(_, symbol)| symbol.clone()) {
                if let Symbol::Fn { ty, args, generic_params, mut cap, body, is_pub } = s {
                    if let Type::Fn { mut tys, ret } = ty {
                        let compiled = self.compile_fn(&args, &mut tys, body.as_ref().clone(), &mut cap)?;
                        for s in compiled.iter() {
                            log::debug!("{}", s);
                        }
                        self.symbols.symbols[id as usize] = Symbol::Fn { ty: Type::Fn { tys, ret }, args, generic_params, cap, body: Arc::new(Stmt::new(StmtKind::Block(compiled), Span::default())), is_pub };
                        fn_ids.push(id);
                    }
                }
            }
        }
        self.symbols.pop_module();
        Ok(fn_ids)
    }

    fn pat_to_var(&mut self, pat: Pattern, expr_ty: Type) -> Result<Pattern> {
        match pat.kind {
            PatternKind::Var { idx, ty } => Ok(Pattern { kind: PatternKind::Var { idx, ty }, span: pat.span }),
            PatternKind::Ident { name, ty } => {
                let ty = self.symbols.get_type(&ty)?;
                let ty = if ty.is_any() { expr_ty } else { ty };
                self.add_ty(ty.clone());
                Ok(Pattern { kind: PatternKind::Var { idx: self.add_name(name), ty }, span: pat.span })
            }
            PatternKind::Tuple(pats) => {
                if let Type::Tuple(tys) = &expr_ty {
                    let pats: Vec<Pattern> = pats.into_iter().zip(tys).filter_map(|p| self.pat_to_var(p.0, p.1.clone()).ok()).collect();
                    if pats.len() == tys.len() { Ok(Pattern { kind: PatternKind::Tuple(pats), span: pat.span }) } else { Err(Self::semantic_error(pat.span, format!("模式与元组类型不匹配: {:?}", expr_ty))) }
                } else {
                    let pats = pats.into_iter().filter_map(|p| self.pat_to_var(p, Type::Any).ok()).collect();
                    Ok(Pattern { kind: PatternKind::Tuple(pats), span: pat.span })
                }
            }
            PatternKind::List { elems, has_rest } => {
                if expr_ty.is_any() {
                    let elems: Vec<Pattern> = elems.into_iter().filter_map(|p| self.pat_to_var(p, Type::Any).ok()).collect();
                    Ok(Pattern { kind: PatternKind::List { elems, has_rest }, span: pat.span })
                } else if let Type::List(elem_ty) | Type::Array(elem_ty, _) | Type::Vec(elem_ty, _) = &expr_ty {
                    let elems: Vec<Pattern> = elems.into_iter().filter_map(|p| self.pat_to_var(p, elem_ty.as_ref().clone()).ok()).collect();
                    Ok(Pattern { kind: PatternKind::List { elems, has_rest }, span: pat.span })
                } else {
                    Err(Self::semantic_error(pat.span, format!("列表模式 {:?} 与类型 {:?} 不匹配", elems, expr_ty)))
                }
            }
            PatternKind::Wildcard => {
                self.add_ty(expr_ty.clone());
                Ok(Pattern { kind: PatternKind::Var { idx: self.add_name(SmolStr::new_static("")), ty: expr_ty }, span: pat.span })
            }
            _ => panic!("未知的模式 {:?}", pat),
        }
    }

    fn infer_range_type(&self, range: &Expr) -> Type {
        if let ExprKind::Range { start, stop, .. } = &range.kind {
            let start_ty = start.get_type();
            let stop_ty = stop.get_type();
            if start_ty.is_any() {
                stop_ty
            } else if stop_ty.is_any() {
                start_ty
            } else if start_ty == Type::I32 && stop_ty.is_uint() {
                stop_ty
            } else if stop_ty == Type::I32 && start_ty.is_uint() {
                start_ty
            } else {
                start_ty + stop_ty
            }
        } else {
            range.get_type()
        }
    }

    fn dyn_init(&mut self, expr: Expr, stmts: &mut Vec<Stmt>, items: Vec<(Expr, Expr)>, ty: Type) -> Expr {
        self.add_name("".into());
        let temp = self.add_ty(ty);
        let span = expr.span;
        stmts.push(Stmt::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Var(temp), span)), op: BinaryOp::Assign, right: Box::new(expr) }, span), true), span));
        for (idx, item) in items {
            let item_span = idx.span.merge(item.span);
            let left = Expr::new(ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Var(temp), item_span)), op: BinaryOp::Idx, right: Box::new(idx) }, item_span);
            stmts.push(Stmt::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Assign, right: Box::new(item) }, item_span), false), item_span));
        }
        Expr::new(ExprKind::Var(temp), span)
    }

    fn is_spawn_closure_call(obj: &Expr, params: &[Expr]) -> bool {
        params.len() == 2 && matches!(&obj.kind, ExprKind::Ident(name) if name.as_str() == "spawn") && matches!(&params[0].kind, ExprKind::Closure { .. })
    }

    fn eval_spawn_arg_pack(&mut self, expr: &Expr, stmts: &mut Vec<Stmt>, cap: &mut Capture) -> Result<Expr> {
        match &expr.kind {
            ExprKind::Tuple(items) | ExprKind::List(items) => Ok(Expr::new(ExprKind::Tuple(items.iter().map(|item| self.eval(item, stmts, cap)).collect::<Result<Vec<_>>>()?), expr.span)),
            _ => Err(Self::semantic_error(expr.span, "spawn closure args must be tuple")),
        }
    }

    fn is_multi_assign_target(expr: &Expr) -> bool {
        matches!(expr.kind, ExprKind::Tuple(_) | ExprKind::List(_))
    }

    fn push_assign(stmts: &mut Vec<Stmt>, left: Expr, right: Expr, span: Span) {
        stmts.push(Stmt::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Assign, right: Box::new(right) }, span), true), span));
    }

    fn temp_var(&mut self, ty: Type, span: Span) -> Expr {
        self.add_name("".into());
        let idx = self.add_ty(ty);
        Expr::new(ExprKind::Var(idx), span)
    }

    fn typed_expr(value: Expr, ty: &Type) -> Expr {
        if ty.is_any() {
            value
        } else {
            let span = value.span;
            Expr::new(ExprKind::Typed { value: Box::new(value), ty: ty.clone() }, span)
        }
    }

    fn lower_multi_assign(&mut self, left: &Expr, right: &Expr, stmts: &mut Vec<Stmt>, cap: &mut Capture, span: Span) -> Result<Expr> {
        let left_items = match &left.kind {
            ExprKind::Tuple(items) | ExprKind::List(items) => items,
            _ => return Err(Self::semantic_error(left.span, "多重赋值左侧必须是 tuple 或 list")),
        };
        if left_items.is_empty() {
            return Err(Self::semantic_error(left.span, "多重赋值左侧不能为空"));
        }

        let mut temps = Vec::with_capacity(left_items.len());
        if let ExprKind::Tuple(right_items) | ExprKind::List(right_items) = &right.kind {
            if left_items.len() != right_items.len() {
                return Err(Self::semantic_error(span, format!("多重赋值数量不匹配: 左侧 {} 个，右侧 {} 个", left_items.len(), right_items.len())));
            }
            for item in right_items {
                let value = self.eval(item, stmts, cap)?;
                let ty = self.infer_expr(&value)?;
                let temp = self.temp_var(ty.clone(), item.span);
                Self::push_assign(stmts, temp.clone(), Self::typed_expr(value, &ty), item.span);
                temps.push((temp, ty));
            }
        } else {
            let value = self.eval(right, stmts, cap)?;
            let ty = self.infer_expr(&value)?;
            let source = self.temp_var(ty.clone(), right.span);
            Self::push_assign(stmts, source.clone(), Self::typed_expr(value, &ty), right.span);
            for idx in 0..left_items.len() {
                let item_span = left_items[idx].span;
                let item = Expr::new(ExprKind::Binary { left: Box::new(source.clone()), op: BinaryOp::Idx, right: Box::new(Expr::new(ExprKind::Value((idx as u32).into()), item_span)) }, item_span);
                let value = self.eval(&item, stmts, cap)?;
                let ty = self.infer_expr(&value)?;
                let temp = self.temp_var(ty.clone(), item_span);
                Self::push_assign(stmts, temp.clone(), Self::typed_expr(value, &ty), item_span);
                temps.push((temp, ty));
            }
        }

        for (target, (temp, ty)) in left_items.iter().zip(temps.iter()) {
            let target = self.eval(target, stmts, cap)?;
            let assign_span = target.span.merge(temp.span);
            Self::push_assign(stmts, target, Self::typed_expr(temp.clone(), ty), assign_span);
        }

        Ok(temps.last().map(|(temp, ty)| Self::typed_expr(temp.clone(), ty)).unwrap_or_else(|| Expr::new(ExprKind::Value(Dynamic::Null), span)))
    }

    fn static_composite_literal(&self, expr: &Expr) -> Result<Option<Dynamic>> {
        match &expr.kind {
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    let Some(value) = self.static_literal_value(item)? else {
                        return Ok(None);
                    };
                    values.push(value);
                }
                Ok(Some(Dynamic::list(values)))
            }
            ExprKind::Dict(items) => {
                let mut values = BTreeMap::new();
                for (key, item) in items {
                    let Some(value) = self.static_literal_value(item)? else {
                        return Ok(None);
                    };
                    values.insert(key.clone(), value);
                }
                Ok(Some(Dynamic::map(values)))
            }
            _ => Ok(None),
        }
    }

    fn static_literal_value(&self, expr: &Expr) -> Result<Option<Dynamic>> {
        match &expr.kind {
            ExprKind::Value(value) => Ok(Some(value.clone())),
            ExprKind::Const(idx) => Ok(self.consts.get(*idx).cloned()),
            ExprKind::Typed { value, ty } if ty.is_native() => Ok(self.static_literal_value(value)?.map(|value| ty.force(value)).transpose()?),
            _ => self.static_composite_literal(expr),
        }
    }

    fn const_expr_value(&self, expr: &Expr) -> Result<Dynamic> {
        match &expr.kind {
            ExprKind::Value(value) => Ok(value.clone()),
            ExprKind::Const(idx) => self.consts.get(*idx).cloned().ok_or_else(|| Self::semantic_error(expr.span, format!("常量索引 {} 不存在", idx))),
            ExprKind::Ident(ident) => {
                let id = self.symbols.get_id(ident).map_err(|_| Self::semantic_error(expr.span, format!("未找到常量 {}", ident)))?;
                match self.symbols.get_symbol(id).map(|(_, symbol)| symbol) {
                    Ok(Symbol::Const { value, .. }) => Ok(value.clone()),
                    Ok(Symbol::Static { value: Some(value), .. }) => Ok(value.clone()),
                    _ => Err(Self::semantic_error(expr.span, format!("{} 不是可用于 const 的静态值", ident))),
                }
            }
            ExprKind::Typed { value, ty } if ty.is_native() => Ok(ty.force(self.const_expr_value(value)?)?),
            ExprKind::Typed { value, .. } => self.const_expr_value(value),
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                let values = items.iter().map(|item| self.const_expr_value(item)).collect::<Result<Vec<_>>>()?;
                Ok(Dynamic::list(values))
            }
            ExprKind::Dict(items) => {
                let mut values = BTreeMap::new();
                for (key, item) in items {
                    values.insert(key.clone(), self.const_expr_value(item)?);
                }
                Ok(Dynamic::map(values))
            }
            ExprKind::Unary { op, value } => {
                let value = self.const_expr_value(value)?;
                match op {
                    parser::UnaryOp::Neg => Ok(-value),
                    parser::UnaryOp::Not => Ok(!value),
                    parser::UnaryOp::Unknow => Err(Self::semantic_error(expr.span, "const 一元表达式无法在编译期求值")),
                }
            }
            ExprKind::Binary { left, op, right } => {
                let left = Expr::new(ExprKind::Value(self.const_expr_value(left)?), left.span);
                let right = Expr::new(ExprKind::Value(self.const_expr_value(right)?), right.span);
                Expr::new(ExprKind::Binary { left: Box::new(left), op: op.clone(), right: Box::new(right) }, expr.span).compact().ok_or_else(|| Self::semantic_error(expr.span, "const 二元表达式无法在编译期求值"))
            }
            _ => Err(Self::semantic_error(expr.span, "const 只能使用字面量、已声明常量和静态 composite literal")),
        }
    }

    fn eval_stmt_expr(&mut self, stmt: &Stmt, stmts: &mut Vec<Stmt>, cap: &mut Capture, span: Span) -> Result<Expr> {
        self.compile_stmt(stmt.clone(), stmts, cap)?;
        let expr_ty = if let Some(stmt) = stmts.last() { if let StmtKind::Expr(expr, _) = &stmt.kind { self.infer_expr(expr)? } else { self.infer_stmt(stmt)? } } else { Type::Any };
        self.add_name("".into());
        let temp = self.add_ty(expr_ty.clone());
        let pat = Pattern { kind: PatternKind::Var { idx: temp, ty: expr_ty }, span };
        stmts.last_mut().ok_or_else(|| Self::semantic_error(span, "没有生成可求值语句表达式")).and_then(|stmt| stmt.bind_pattern(pat))?;
        Ok(Expr::new(ExprKind::Var(temp), span))
    }

    fn eval(&mut self, expr: &Expr, stmts: &mut Vec<Stmt>, cap: &mut Capture) -> Result<Expr> {
        match &expr.kind {
            ExprKind::Stmt(stmt) => self.eval_stmt_expr(stmt, stmts, cap, expr.span),
            ExprKind::Closure { args, body } => {
                let (mut names, mut tys): (Vec<SmolStr>, Vec<Type>) = args.clone().into_iter().unzip();
                let top = self.top();
                let mut cap_vars: Vec<(SmolStr, Type)> = self.names[top..].iter().zip(self.tys[top..].iter()).map(|(n, ty)| (n.clone(), ty.clone())).collect();
                let parent_cap_start = cap_vars.len();
                cap_vars.extend(cap.names.iter().cloned());
                let mut local_cap = Capture::new(cap_vars);
                let _ = self.compile_fn(names.as_slice(), &mut tys.clone(), *body.clone(), &mut local_cap)?;
                for cap_idx in local_cap.vars.iter() {
                    if *cap_idx >= parent_cap_start {
                        let _ = cap.get(&local_cap.names[*cap_idx].0);
                    }
                    names.push(local_cap.names[*cap_idx].0.clone());
                    tys.push(local_cap.names[*cap_idx].1.clone());
                }
                let mut compiled = self.compile_fn(names.as_slice(), &mut tys.clone(), *body.clone(), &mut Capture::default())?;
                let (ty, args) = Type::from_args(args.clone());
                let body_stmt = if compiled.len() == 1 { compiled.pop().unwrap() } else { Stmt::new(StmtKind::Block(compiled), expr.span) };
                let name = SmolStr::from(format!("__closure_{}_{}", expr.span.start, expr.span.end));
                let fn_id = self.symbols.add(name, Symbol::Fn { ty, args, generic_params: Vec::new(), cap: local_cap, body: Arc::new(body_stmt), is_pub: false });
                Ok(Expr::new(ExprKind::Id(fn_id, None), expr.span))
            }
            ExprKind::Value(v) => {
                if v.is_native() {
                    Ok(Expr::new(ExprKind::Value(v.clone()), expr.span))
                } else {
                    Ok(Expr::new(ExprKind::Const(self.get_const(v.clone())), expr.span))
                }
            }
            ExprKind::Typed { value, ty } => {
                let ty = self.symbols.get_type(ty)?;
                if let Type::Struct { fields, .. } = &ty
                    && let ExprKind::Dict(dict) = &value.kind
                {
                    let mut items = Vec::new();
                    for field in fields {
                        if let Some((_, v)) = dict.iter().find(|(name, _)| name == &field.0) {
                            items.push(self.eval(v, stmts, cap)?);
                        }
                    }
                    Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::List(items), expr.span)), ty }, expr.span))
                } else if let Type::Struct { .. } = &ty
                    && let ExprKind::List(list) = &value.kind
                {
                    let items = list.iter().map(|item| self.eval(item, stmts, cap)).collect::<Result<Vec<_>>>()?;
                    Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::List(items), expr.span)), ty }, expr.span))
                } else if let Type::Array(_, _) = &ty
                    && let ExprKind::List(list) = &value.kind
                {
                    let items = list.iter().map(|item| self.eval(item, stmts, cap)).collect::<Result<Vec<_>>>()?;
                    Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::List(items), expr.span)), ty }, expr.span))
                } else if value.is_value() {
                    let value = value.clone().value()?;
                    if ty.is_str() && value.is_str() {
                        log::warn!("常量 String 只能作为动态值使用，已忽略 string 类型约束");
                        Ok(Expr::new(ExprKind::Const(self.get_const(value)), expr.span))
                    } else {
                        Ok(Expr::new(ExprKind::Value(ty.force(value)?), expr.span))
                    }
                } else {
                    Ok(Expr::new(ExprKind::Typed { value: Box::new(self.eval(value, stmts, cap)?), ty }, expr.span))
                }
            }
            ExprKind::Ident(ident) => {
                // 局部变量 → 捕获变量 → 全局符号
                for idx in (self.top()..self.names.len()).rev() {
                    if self.names[idx].eq(ident) {
                        return Ok(Expr::new(ExprKind::Var((idx - self.top()) as u32), expr.span));
                    }
                }
                if let Some(idx) = cap.get(ident) {
                    return Ok(Expr::new(ExprKind::Capture(idx as u32), expr.span));
                }
                self.get_ident(ident, expr.span)
            },
            ExprKind::Generic { obj, params } => {
                let obj = self.eval(obj, stmts, cap)?;
                let params = params.iter().map(|param| self.symbols.get_type(param).unwrap_or_else(|_| param.clone())).collect();
                match obj.kind {
                    ExprKind::Id(id, None) | ExprKind::AssocId { id, .. } => Ok(Expr::new(ExprKind::AssocId { id, params }, expr.span)),
                    _ => Err(Self::semantic_error(expr.span, format!("范型参数只能用于函数或关联函数调用: {:?}", obj))),
                }
            }
            ExprKind::Assoc { ty, name } => {
                let base_name = match ty {
                    Type::Ident { name, .. } => name.clone(),
                    Type::Symbol { id, .. } => self.symbols.get_symbol(*id)?.0.clone(),
                    _ => return Err(Self::semantic_error(expr.span, format!("关联函数目标必须是类型: {:?}", ty))),
                };
                let id = self.symbols.get_id(&format!("{}::{}", base_name, name)).map_err(|_| Self::semantic_error(expr.span, format!("未找到关联函数 {}::{}", base_name, name)))?;
                let params = match ty {
                    Type::Ident { params, .. } | Type::Symbol { params, .. } => params.iter().map(|param| self.symbols.get_type(param).unwrap_or_else(|_| param.clone())).collect(),
                    _ => Vec::new(),
                };
                Ok(Expr::new(ExprKind::AssocId { id, params }, expr.span))
            }
            ExprKind::Unary { op, value } => {
                let value = Expr::new(ExprKind::Unary { op: op.clone(), value: Box::new(self.eval(value, stmts, cap)?) }, expr.span);
                if let Some(v) = value.compact() { Ok(Expr::new(ExprKind::Value(v), expr.span)) } else { Ok(value) }
            }
            ExprKind::Binary { left, op, right } => {
                if *op == BinaryOp::Assign && Self::is_multi_assign_target(left) {
                    return self.lower_multi_assign(left, right, stmts, cap, expr.span);
                }
                let left = self.eval(left, stmts, cap)?;
                if *op == BinaryOp::Idx {
                    if let Some(key) = self.get_value(right).and_then(|v| if v.is_str() { Some(v.as_str().to_string()) } else { None }) {
                        if let Some(field) = self.type_field_access_expr(left.clone(), &key, expr.span, true) {
                            return Ok(field);
                        }
                        return Ok(self.literal_field_access_expr(left, &key, expr.span));
                    } else if let Ok(ident) = right.ident() {
                        if let Ok(found) = self.get_ident(ident, right.span) {
                            return Ok(if let Some(id) = found.id() {
                                Expr::new(ExprKind::Id(id, Some(Box::new(left))), expr.span)
                            } else {
                                Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(found) }, expr.span)
                            });
                        }
                        if let Ok(ty) = self.infer_expr(&left)
                            && let Ok((idx, ty)) = self.get_field(&ty, ident)
                        {
                            return Ok(if let Type::Symbol { id, .. } = ty {
                                Expr::new(ExprKind::Id(id, Some(Box::new(left))), expr.span)
                            } else if ty.is_bool() && idx == usize::MAX {
                                Expr::new(ExprKind::Value(Dynamic::Bool(false)), expr.span)
                            } else if ty.is_any() && idx == usize::MAX {
                                let right = Expr::new(ExprKind::Const(self.get_const(Dynamic::String(ident.into()))), expr.span);
                                Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(right) }, expr.span)
                            } else {
                                Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(Expr::new(ExprKind::Value(Dynamic::U32(idx as u32)), expr.span)) }, expr.span)
                            });
                        } else {
                            let right = Expr::new(ExprKind::Const(self.get_const(Dynamic::String(ident.into()))), expr.span);
                            return Ok(Expr::new(ExprKind::Binary { left: Box::new(left), op: BinaryOp::Idx, right: Box::new(right) }, expr.span));
                        }
                    }
                }
                let right = self.eval(right, stmts, cap)?;
                let value = Self::normalize_self_assign(left, op.clone(), right, expr.span, self.arg_counts.last().copied().unwrap_or(0));
                if let Some(v) = value.compact() { Ok(Expr::new(ExprKind::Value(v), expr.span)) } else { Ok(value) }
            }
            ExprKind::Call { obj, params } => {
                let params: Vec<Expr> = if Self::is_spawn_closure_call(obj, params) {
                    vec![self.eval(&params[0], stmts, cap)?, self.eval_spawn_arg_pack(&params[1], stmts, cap)?]
                } else {
                    params.iter().map(|p| self.eval(p, stmts, cap)).collect::<Result<Vec<_>>>()?
                };
                let obj_result = if let Some(method_obj) = self.method_call_obj_expr(obj, stmts, cap)? { Ok(method_obj) } else { self.eval(obj, stmts, cap) };
                match obj_result {
                    Ok(obj) if obj.is_value() && params.is_empty() => Ok(obj),
                    Ok(obj) => Ok(Expr::new(ExprKind::Call { obj: Box::new(obj), params }, expr.span)),
                    Err(e) => {
                        if let ExprKind::Ident(ident) = &obj.kind {
                            let fn_id = if ident.contains("::") { self.symbols.add_global(ident.clone(), Symbol::Null) } else { self.symbols.add(ident.clone(), Symbol::Null) };
                            Ok(Expr::new(ExprKind::Call { obj: Box::new(Expr::new(ExprKind::Id(fn_id, None), obj.span)), params }, expr.span))
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            ExprKind::Range { start, stop, inclusive } => {
                let start = Box::new(self.eval(start, stmts, cap)?);
                let stop = Box::new(self.eval(stop, stmts, cap)?);
                Ok(Expr::new(ExprKind::Range { start, stop, inclusive: *inclusive }, expr.span))
            }
            ExprKind::List(list) | ExprKind::Tuple(list) => {
                if let Some(value) = self.static_composite_literal(expr)? {
                    let idx = self.get_const(value);
                    return Ok(Expr::new(ExprKind::Const(idx), expr.span));
                }
                let mut v = Vec::new();
                let mut items = Vec::new();
                for (idx, item) in list.iter().enumerate() {
                    if item.is_value() {
                        v.push(item.clone().value().unwrap());
                    } else {
                        items.push((Expr::new(ExprKind::Value((idx as u32).into()), item.span), self.eval(item, stmts, cap)?));
                        v.push(Dynamic::Null);
                    }
                }
                let list = Expr::new(ExprKind::Const(self.get_const(Dynamic::list(v))), expr.span);
                Ok(self.dyn_init(list, stmts, items, Type::Any))
            }
            ExprKind::Repeat { value, len } => {
                let len = self.symbols.get_type(len)?;
                let Type::ConstInt(len) = len else {
                    return Err(Self::semantic_error(expr.span, format!("重复数组长度必须是编译期整数: {:?}", len)));
                };
                if len < 0 {
                    return Err(Self::semantic_error(expr.span, "重复数组长度不能为负数"));
                }
                Ok(Expr::new(ExprKind::Repeat { value: Box::new(self.eval(value, stmts, cap)?), len: Type::ConstInt(len) }, expr.span))
            }
            ExprKind::Dict(dict) => {
                if let Some(value) = self.static_composite_literal(expr)? {
                    let idx = self.get_const(value);
                    return Ok(Expr::new(ExprKind::Const(idx), expr.span));
                }
                let mut dyn_kv = Vec::new();
                let mut m = BTreeMap::new();
                for (k, v) in dict {
                    if v.is_value() {
                        m.insert(k.clone(), v.clone().value()?);
                    } else {
                        let key = Expr::new(ExprKind::Const(self.get_const(Dynamic::String(k.clone()))), v.span);
                        dyn_kv.push((key, self.eval(v, stmts, cap)?));
                        m.insert(k.clone(), Dynamic::Null);
                    }
                }
                let dict = Expr::new(ExprKind::Const(self.get_const(Dynamic::map(m))), expr.span);
                Ok(self.dyn_init(dict, stmts, dyn_kv, Type::Any))
            }
            ExprKind::Id(_, _) | ExprKind::AssocId { .. } => Ok(expr.clone()),
            _ => Ok(expr.clone()),
        }
    }

    fn get_stmt(&mut self, stmt: Stmt, cap: &mut Capture) -> Result<Stmt> {
        let span = stmt.span;
        let mut stmts = Vec::new();
        self.compile_stmt(stmt, &mut stmts, cap)?;
        Ok(Stmt::new(StmtKind::Block(stmts), span))
    }

    fn compile_stmt(&mut self, stmt: Stmt, compiled: &mut Vec<Stmt>, cap: &mut Capture) -> Result<()> {
        let stmt_span = stmt.span;
        match stmt.kind {
            StmtKind::Let { mut pat, value } => {
                let value = *value;
                let string_literal_constraint = matches!(
                    (&pat.kind, &value.kind),
                    (
                        PatternKind::Ident { ty: Type::Str, .. },
                        StmtKind::Expr(
                            Expr {
                                kind: ExprKind::Value(value),
                                ..
                            },
                            _
                        )
                    ) if value.is_str()
                );
                if string_literal_constraint {
                    log::warn!("常量 String 只能作为动态值使用，已忽略 string 类型约束");
                    if let PatternKind::Ident { ty, .. } = &mut pat.kind {
                        *ty = Type::Any;
                    }
                }
                let annotated_ty = if let PatternKind::Ident { ty, .. } = &pat.kind {
                    let ty = self.symbols.get_type(ty)?;
                    if ty.is_any() { None } else { Some(ty) }
                } else {
                    None
                };
                let pattern_expr_ty = if matches!(pat.kind, PatternKind::List { .. } | PatternKind::Tuple(_)) { if let StmtKind::Expr(expr, _) = &value.kind { Some(self.infer_expr(expr)?) } else { None } } else { None };
                if let Some(ty) = annotated_ty {
                    if let StmtKind::Expr(expr, close) = value.kind {
                        let span = expr.span;
                        let typed = Expr::new(ExprKind::Typed { value: Box::new(expr), ty }, span);
                        self.compile_stmt(Stmt::new(StmtKind::Expr(typed, close), value.span), compiled, cap)?;
                    } else {
                        self.compile_stmt(value, compiled, cap)?;
                    }
                } else {
                    self.compile_stmt(value, compiled, cap)?;
                }
                let expr_ty = if let Some(ty) = pattern_expr_ty {
                    ty
                } else if let Some(stmt) = compiled.last() {
                    if let StmtKind::Expr(expr, _) = &stmt.kind { self.infer_expr(expr)? } else { self.infer_stmt(stmt)? }
                } else {
                    Type::Any
                };
                let pat = self.pat_to_var(pat, expr_ty)?;
                compiled.last_mut().ok_or_else(|| Self::semantic_error(stmt_span, "没有生成可绑定模式的编译语句")).and_then(|stmt| stmt.bind_pattern(pat))?;
            }
            StmtKind::Expr(expr, close) => {
                if let ExprKind::Binary { left, op: BinaryOp::Assign, right } = &expr.kind
                    && Self::is_multi_assign_target(left)
                {
                    self.lower_multi_assign(left, right, compiled, cap, stmt_span)?;
                    return Ok(());
                }
                let e = self.eval(&expr, compiled, cap)?;
                compiled.push(Stmt::new(StmtKind::Expr(e, close), stmt_span));
            }
            StmtKind::Block(stmts) => {
                let mut block = Vec::new();
                for stmt in stmts {
                    self.compile_stmt(stmt, &mut block, cap)?;
                }
                compiled.push(Stmt::new(StmtKind::Block(block), stmt_span));
            }
            StmtKind::Fn { name, generic_params, args, body, is_pub } => {
                let (ty, args) = Type::from_args(args);
                if let Type::Fn { mut tys, ret } = ty {
                    let mut fn_cap = Capture::default();
                    let compiled_body = self.compile_fn(&args, &mut tys, *body, &mut fn_cap)?;
                    self.symbols.add(name, Symbol::Fn { ty: Type::Fn { tys, ret }, args, generic_params, cap: fn_cap, body: Arc::new(Stmt::new(StmtKind::Block(compiled_body), stmt_span)), is_pub });
                } else {
                    panic!("nested functions are not supported here")
                }
            }
            StmtKind::Return(expr) => {
                let expr = expr.and_then(|e| self.eval(&e, compiled, cap).ok());
                compiled.push(Stmt::new(StmtKind::Return(expr), stmt_span));
            }
            StmtKind::If { cond, then_body, else_body } => {
                let cond = self.eval(&cond, compiled, cap)?;
                if let Some(cond_value) = cond.compact()
                    && let Some(cond_bool) = cond_value.as_bool()
                {
                    if cond_bool {
                        self.compile_stmt(*then_body, compiled, cap)?;
                    } else if let Some(body) = else_body {
                        self.compile_stmt(*body, compiled, cap)?;
                    }
                } else {
                    let then_body = Box::new(self.get_stmt(*then_body, cap)?);
                    let else_body = if let Some(body) = else_body { Some(Box::new(self.get_stmt(*body, cap)?)) } else { None };
                    compiled.push(Stmt::new(StmtKind::If { cond, then_body, else_body }, stmt_span));
                }
            }
            StmtKind::Loop(body) => {
                compiled.push(Stmt::new(StmtKind::Loop(Box::new(self.get_stmt(*body, cap)?)), stmt_span));
            }
            StmtKind::While { cond, body } => {
                let cond = self.eval(&cond, compiled, cap)?;
                compiled.push(Stmt::new(StmtKind::While { cond, body: Box::new(self.get_stmt(*body, cap)?) }, stmt_span));
            }
            StmtKind::For { pat, range, body } => {
                let range = self.eval(&range, compiled, cap)?;
                let range_ty = self.infer_range_type(&range);
                let pat = self.pat_to_var(pat, range_ty)?;
                compiled.push(Stmt::new(StmtKind::For { pat, range, body: Box::new(self.get_stmt(*body, cap)?) }, stmt_span));
            }
            stmt_kind => {
                compiled.push(Stmt::new(stmt_kind, stmt_span));
            }
        }
        Ok(())
    }
}
