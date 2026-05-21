use crate::{JITRunTime, context::BuildContext, rt::PendingFn};
use anyhow::{Context, Result, anyhow};
use compiler::{Symbol, infer_generic_args_from_types, substitute_stmt, substitute_type};
use cranelift::{codegen::ir::FuncRef, prelude::*};
use cranelift_module::{FuncId, Module};
use dynamic::Type;

#[derive(Debug)]
pub enum FnVariant {
    Native { ty: Type, fn_id: FuncId, context: Option<usize> },                                                        //没有变体 直接调用的原生函数
    Inline { fn_ptr: fn(Option<&mut BuildContext>, Vec<Value>) -> Result<(Option<Value>, Type)>, arg_tys: Vec<Type> }, //inline 函数 直接生成代码
    Compiled(Vec<(Type, FuncId)>),
}

impl FnVariant {
    pub fn is_compiled(&self) -> bool {
        if let Self::Compiled(_) = self { true } else { false }
    }
}

use crate::get_type;
use cranelift_module::Linkage;
use parser::{Expr, ExprKind, Span, Stmt, StmtKind};
use smol_str::SmolStr;

#[derive(Debug)]
pub enum FnInfo {
    //用来调用的函数信息
    Call { fn_id: FuncId, arg_tys: Vec<Type>, caps: Vec<usize>, ret: Type, context: Option<usize> },
    Inline { fn_ptr: fn(Option<&mut BuildContext>, Vec<Value>) -> Result<(Option<Value>, Type)>, arg_tys: Vec<Type> },
}

impl FnInfo {
    pub fn get_id(&self) -> Result<FuncId> {
        if let Self::Call { fn_id, arg_tys: _, caps: _, ret: _, context: _ } = self { Ok(*fn_id) } else { Err(anyhow!("Inline 函数没有 FuncId")) }
    }

    pub fn arg_tys(&self) -> Result<&[Type]> {
        match self {
            Self::Call { fn_id: _, arg_tys, caps: _, ret: _, context: _ } => Ok(arg_tys),
            Self::Inline { fn_ptr: _, arg_tys } => Ok(arg_tys),
        }
    }

    pub fn get_type(&self) -> Result<Type> {
        match self {
            Self::Call { fn_id: _, arg_tys: _, caps: _, ret, context: _ } => Ok(ret.clone()),
            Self::Inline { fn_ptr, arg_tys: _ } => fn_ptr(None, vec![]).map(|(_, t)| t),
        }
    }
}

impl JITRunTime {
    fn coerce_returns(stmt: &Stmt, ret_ty: &Type) -> Stmt {
        let kind = match &stmt.kind {
            StmtKind::Return(Some(expr)) if ret_ty.is_void() => StmtKind::Return(None),
            StmtKind::Return(Some(expr)) => StmtKind::Return(Some(Expr::new(ExprKind::Typed { value: Box::new(expr.clone()), ty: ret_ty.clone() }, expr.span))),
            StmtKind::Block(stmts) => {
                let last = stmts.len().saturating_sub(1);
                StmtKind::Block(stmts.iter().enumerate().map(|(idx, stmt)| if idx == last { Self::coerce_returns(stmt, ret_ty) } else { stmt.clone() }).collect())
            }
            StmtKind::If { cond, then_body, else_body } => {
                StmtKind::If { cond: cond.clone(), then_body: Box::new(Self::coerce_returns(then_body, ret_ty)), else_body: else_body.as_ref().map(|body| Box::new(Self::coerce_returns(body, ret_ty))) }
            }
            StmtKind::While { cond, body } => StmtKind::While { cond: cond.clone(), body: Box::new(Self::coerce_returns(body, ret_ty)) },
            StmtKind::Loop(body) => StmtKind::Loop(Box::new(Self::coerce_returns(body, ret_ty))),
            StmtKind::For { pat, range, body } => StmtKind::For { pat: pat.clone(), range: range.clone(), body: Box::new(Self::coerce_returns(body, ret_ty)) },
            _ => stmt.kind.clone(),
        };
        Stmt::new(kind, stmt.span)
    }

    pub fn add_inline(&mut self, name: &str, args: Vec<Type>, ret: Type, f: fn(Option<&mut BuildContext>, Vec<Value>) -> Result<(Option<Value>, Type)>) -> Result<u32> {
        let id = self.compiler.add_symbol(name, Symbol::native(args.clone(), ret));
        self.fns.insert(id, FnVariant::Inline { fn_ptr: f.into(), arg_tys: args });
        if let Some((def, method)) = name.split_once("::") {
            let def_id = self.get_id(def)?;
            if let Some((_, define)) = self.compiler.symbols.get_symbol_mut(def_id) {
                if let Symbol::Struct(Type::Struct { params, fields }, _) = define {
                    fields.push((method.into(), Type::Symbol { id, params: params.clone() }));
                }
            }
        }
        Ok(id)
    }

    pub fn get_fn_ref(&mut self, ctx: &mut BuildContext, fn_id: FuncId) -> FuncRef {
        ctx.get_fn_ref(fn_id).unwrap_or_else(|| {
            let fn_ref = self.module.declare_func_in_func(fn_id, &mut ctx.builder.func);
            ctx.fn_refs.push((fn_id, fn_ref));
            fn_ref
        })
    }

    pub fn adjust_args(&mut self, ctx: &mut BuildContext, args: Vec<(Value, Type)>, arg_tys: &[Type]) -> Result<Vec<Value>> {
        let mut results = Vec::<Value>::new();
        for ((arg, ty), arg_ty) in args.into_iter().zip(arg_tys.iter()) {
            if ty != *arg_ty {
                results.push(self.convert(ctx, (arg, ty), arg_ty.clone())?);
            } else {
                results.push(arg);
            }
        }
        Ok(results)
    }

    pub fn get_fn(&self, id: u32, want_tys: &[Type]) -> Result<FnInfo> {
        if let Some(fn_info) = self.fns.get(&id) {
            match fn_info {
                FnVariant::Compiled(fns) => {
                    for (ty, fn_id) in fns.iter() {
                        if let Type::Fn { tys, ret } = ty.clone() {
                            if tys.len() != want_tys.len() {
                                continue;
                            }
                            let mut real_types = Vec::new();
                            for (ty1, ty2) in tys.iter().zip(want_tys.iter()) {
                                if ty1 != ty2 {
                                    if ty1.is_any() || ty2.is_any() {
                                        real_types.push(ty1.clone());
                                    }
                                    //ty1 是目的类型
                                    else {
                                        break;
                                    }
                                } else {
                                    real_types.push(ty1.clone());
                                }
                            }
                            if real_types.len() == want_tys.len() {
                                return Ok(FnInfo::Call { fn_id: *fn_id, arg_tys: real_types, caps: Vec::new(), ret: ret.as_ref().clone(), context: None });
                            }
                        }
                    }
                }
                FnVariant::Inline { fn_ptr, arg_tys } => {
                    return Ok(FnInfo::Inline { fn_ptr: fn_ptr.clone(), arg_tys: arg_tys.clone() });
                }
                FnVariant::Native { ty, fn_id, context } => {
                    if let Type::Fn { tys, ret } = ty.clone() {
                        return Ok(FnInfo::Call { fn_id: *fn_id, arg_tys: tys, caps: Vec::new(), ret: ret.as_ref().clone(), context: *context });
                    }
                }
            }
        }
        Err(anyhow!("未发现函数 {}", id))
    }

    pub fn get_sig(&mut self, arg_tys: &[Type], ret: Type) -> Result<Signature> {
        if let Some(st) = self.sigs.iter().find_map(|s| if s.0 == arg_tys && ret == s.2 { Some(s.1.clone()) } else { None }) {
            return Ok(st);
        }
        let mut sig = self.module.make_signature();
        for arg in arg_tys.iter() {
            sig.params.push(AbiParam::new(get_type(arg)?));
        }
        if !ret.is_void() {
            sig.returns.push(AbiParam::new(get_type(&ret)?));
        }
        self.sigs.push((arg_tys.to_vec(), sig.clone(), ret.clone()));
        Ok(sig)
    }

    fn declare_compiled_fn(&mut self, name_id: Option<&(SmolStr, u32)>, arg_tys: &[Type], ret_ty: Type) -> Result<FuncId> {
        let sig = self.get_sig(arg_tys, ret_ty.clone())?;
        log::info!("{:?} {:?}", name_id, sig);
        if let Some((name, id)) = name_id {
            let fn_id = self.module.declare_function(&name, Linkage::Local, &sig)?;
            let variant = (Type::Fn { tys: arg_tys.to_vec(), ret: std::rc::Rc::new(ret_ty.clone()) }, fn_id);
            if let Some(FnVariant::Compiled(fns)) = self.fns.get_mut(id) {
                fns.push(variant);
            } else {
                self.fns.insert(*id, FnVariant::Compiled(vec![variant]));
            }
            Ok(fn_id)
        } else {
            Ok(self.module.declare_anonymous_function(&sig)?)
        }
    }

    fn define_compiled_fn(&mut self, fn_id: FuncId, name_id: Option<&(SmolStr, u32)>, arg_tys: &[Type], ret_ty: Type, stmt: &Stmt) -> Result<()> {
        let sig = self.get_sig(arg_tys, ret_ty.clone())?;
        #[cfg(feature = "ir-disassembly")]
        let fn_name = name_id.map(|(name, _)| name.clone());
        let mut ctx = self.module.make_context();
        ctx.func.signature = sig.clone();

        let mut func_ctx = FunctionBuilderContext::new();
        let builder = FunctionBuilder::new(&mut ctx.func, &mut func_ctx);

        let mut build_ctx = BuildContext::new(builder, &arg_tys)?;
        self.compile_depth += 1;
        let stmt = Self::coerce_returns(stmt, &ret_ty);
        let gen_result = self.gen_stmt(&mut build_ctx, &stmt, None, None);
        self.compile_depth -= 1;
        gen_result?;

        build_ctx.builder.seal_all_blocks();
        #[cfg(feature = "ir-disassembly")]
        {
            let ir = format!("{}", ctx.func.display());
            if let Some(name) = fn_name {
                self.ir_disassembly.insert(name, ir);
            }
        }
        self.module.define_function(fn_id, &mut ctx).with_context(|| name_id.map(|(name, _)| format!("define function {}", name)).unwrap_or_else(|| "define anonymous function".to_string()))?;
        log::info!("{:?}", ctx.func);
        Ok(())
    }

    pub(crate) fn compile_fn(&mut self, name_id: Option<(SmolStr, u32)>, arg_tys: &[Type], ret_ty: Type, stmt: &Stmt) -> Result<FuncId> {
        let drain_pending = self.compile_depth == 0;
        let fn_id = self.declare_compiled_fn(name_id.as_ref(), arg_tys, ret_ty.clone())?;
        self.define_compiled_fn(fn_id, name_id.as_ref(), arg_tys, ret_ty, stmt)?;
        if drain_pending {
            self.drain_pending_fns()?;
        }
        Ok(fn_id)
    }

    fn drain_pending_fns(&mut self) -> Result<()> {
        while let Some(pending) = self.pending_fns.pop_front() {
            let name_id = (pending.name, pending.symbol_id);
            self.define_compiled_fn(pending.fn_id, Some(&name_id), &pending.arg_tys, pending.ret_ty, &pending.body)?;
        }
        Ok(())
    }

    pub fn gen_fn(&mut self, ctx: Option<&BuildContext>, id: u32, arg_tys: &[Type]) -> Result<FnInfo> {
        self.gen_fn_with_params(ctx, id, arg_tys, &[])
    }

    pub fn gen_fn_with_params(&mut self, ctx: Option<&BuildContext>, id: u32, arg_tys: &[Type], generic_args: &[Type]) -> Result<FnInfo> {
        let mut arg_tys: Vec<Type> = arg_tys.iter().map(|ty| self.compiler.symbols.get_type(ty).unwrap()).collect();
        if generic_args.is_empty()
            && let Ok(info) = self.get_fn(id, &arg_tys)
        {
            return Ok(info);
        }
        let (name, s) = self.compiler.symbols.get_symbol(id).map(|(n, s)| (n.clone(), s.clone()))?;
        if let Symbol::Fn { ty, args, generic_params, cap, body, is_pub: _ } = s.clone() {
            if let Type::Fn { tys: decl_tys, ret: _ } = ty {
                let inferred_generic_args = if generic_args.is_empty() { infer_generic_args_from_types(&generic_params, &decl_tys, &arg_tys) } else { generic_args.to_vec() };
                let generic_args = if generic_params.is_empty() { &[] } else { inferred_generic_args.as_slice() };
                let decl_tys = if generic_params.is_empty() { decl_tys } else { decl_tys.iter().map(|ty| substitute_type(ty, &generic_params, generic_args)).collect() };
                while arg_tys.len() < decl_tys.len() {
                    arg_tys.push(self.compiler.symbols.get_type(&decl_tys[arg_tys.len()]).unwrap_or(Type::Any));
                }
                let ret_ty = self.compiler.infer_fn_with_params(id, &arg_tys, generic_args)?;
                if let Some(FnVariant::Compiled(fns)) = self.fns.get(&id) {
                    for (ty, fn_id) in fns {
                        if let Type::Fn { tys, ret } = ty
                            && tys == &arg_tys
                            && ret.as_ref() == &ret_ty
                        {
                            return Ok(FnInfo::Call { fn_id: *fn_id, arg_tys: arg_tys.to_vec(), caps: Vec::new(), ret: ret_ty, context: None });
                        }
                    }
                }
                let mut compile_cap = cap.clone();
                let body = if generic_params.is_empty() {
                    body.as_ref().clone()
                } else {
                    let mut compile_tys = decl_tys.clone();
                    let substituted = substitute_stmt(body.as_ref(), &generic_params, generic_args);
                    let saved_state = self.compiler.take_local_state();
                    let compiled_body = self.compiler.compile_fn(&args, &mut compile_tys, substituted, &mut compile_cap);
                    self.compiler.restore_local_state(saved_state);
                    Stmt::new(StmtKind::Block(compiled_body?), Span::default())
                };
                for v in compile_cap.vars.iter() {
                    ctx.as_ref().map(|ctx| arg_tys.push(ctx.vars[*v].get_ty()));
                }
                let fn_id = if self.compile_depth > 0 {
                    let fn_id = self.declare_compiled_fn(Some(&(name.clone(), id)), &arg_tys, ret_ty.clone())?;
                    self.pending_fns.push_back(PendingFn { name: name.clone(), symbol_id: id, fn_id, arg_tys: arg_tys.clone(), ret_ty: ret_ty.clone(), body });
                    fn_id
                } else {
                    let fn_id = self.compile_fn(Some((name.clone(), id)), &arg_tys, ret_ty.clone(), &body)?;
                    self.drain_pending_fns()?;
                    self.module.finalize_definitions()?;
                    fn_id
                };
                return Ok(FnInfo::Call { fn_id, arg_tys: arg_tys.to_vec(), caps: compile_cap.vars.clone(), ret: ret_ty, context: None });
            }
            let ret_ty = self.compiler.infer_fn_with_params(id, &arg_tys, generic_args)?;
            for v in cap.vars.iter() {
                ctx.as_ref().map(|ctx| arg_tys.push(ctx.vars[*v].get_ty()));
            }
            let body = if generic_params.is_empty() { body.as_ref().clone() } else { substitute_stmt(body.as_ref(), &generic_params, generic_args) };
            let fn_id = if self.compile_depth > 0 {
                let fn_id = self.declare_compiled_fn(Some(&(name.clone(), id)), &arg_tys, ret_ty.clone())?;
                self.pending_fns.push_back(PendingFn { name: name.clone(), symbol_id: id, fn_id, arg_tys: arg_tys.clone(), ret_ty: ret_ty.clone(), body });
                fn_id
            } else {
                let fn_id = self.compile_fn(Some((name.clone(), id)), &arg_tys, ret_ty.clone(), &body)?;
                self.drain_pending_fns()?;
                self.module.finalize_definitions()?;
                fn_id
            };
            return Ok(FnInfo::Call { fn_id, arg_tys: arg_tys.to_vec(), caps: cap.vars.clone(), ret: ret_ty, context: None });
        }
        Err(anyhow!("生成函数 {} 失败", id))
    }
}
