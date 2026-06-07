use compiler::{Capture, Compiler, Symbol};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, PatternKind, Span, Stmt, StmtKind, UnaryOp};
use std::collections::{BTreeMap, HashMap, VecDeque};

use crate::context::LocalVar;

use super::{FnInfo, FnVariant, PTR_TYPE, context::BuildContext, get_type, ptr_type};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Module};

use anyhow::{Result, anyhow};
use smol_str::SmolStr;
use std::sync::{Arc, Mutex, RwLock, Weak};

pub struct JITRunTime {
    pub compiler: Compiler,
    pub fns: BTreeMap<u32, FnVariant>,
    pub sigs: Vec<(Vec<Type>, Signature, Type)>,
    pub native_symbols: Arc<RwLock<HashMap<String, usize>>>,
    pub(crate) owner: Weak<Mutex<JITRunTime>>,
    pub(crate) pending_fns: VecDeque<PendingFn>,
    pub(crate) compile_depth: usize,
    #[cfg(feature = "ir-disassembly")]
    pub ir_disassembly: BTreeMap<SmolStr, String>,
    pub module: JITModule,
    pub consts: Vec<Option<usize>>,
    pub(crate) scope_enter_fn: Option<FuncId>,
    pub(crate) scope_exit_void_fn: Option<FuncId>,
    pub(crate) scope_exit_dynamic_fn: Option<FuncId>,
    pub(crate) scope_exit_bytes_fn: Option<FuncId>,
    pub(crate) struct_alloc_fn: Option<FuncId>,
    pub(crate) repeat_fill_fn: Option<FuncId>,
    pub(crate) strcat_fn: Option<FuncId>,
    pub(crate) strcat_i64_fn: Option<FuncId>,
    pub(crate) strcat_assign_fn: Option<FuncId>,
    pub(crate) spawn_ptr_fn: Option<FuncId>,
    pub(crate) struct_from_ptr_fn: Option<FuncId>,
    pub(crate) array_from_ptr_fn: Option<FuncId>,
    pub(crate) array_to_ptr_fn: Option<FuncId>,
}

// TODO(memory): 函数调用期间为 VM 内部临时 Any/struct 分配引入 arena。
// 临时值进入 arena，返回值 promote 给 Rust 调用方；否则需要完整 drop 插桩，
// 覆盖表达式丢弃、变量覆盖、函数出口、break/continue/return 等路径。
pub(crate) struct PendingFn {
    pub name: SmolStr,
    pub symbol_id: u32,
    pub fn_id: FuncId,
    pub arg_tys: Vec<Type>,
    pub ret_ty: Type,
    pub local_type_hints: Vec<Option<Type>>,
    pub body: Stmt,
}

impl JITRunTime {
    fn expr(kind: ExprKind) -> Expr {
        Expr::new(kind, Span::default())
    }

    fn stmt(kind: StmtKind) -> Stmt {
        Stmt::new(kind, Span::default())
    }

    pub(crate) fn type_ptr_const(ctx: &mut BuildContext, ty: &Type) -> Value {
        let ty_ptr = Box::into_raw(Box::new(ty.clone()));
        ctx.builder.ins().iconst(ptr_type(), ty_ptr as i64)
    }

    pub fn load(&mut self, code: Vec<u8>, arg_name: SmolStr) -> Result<(i64, Type)> {
        let stmts = Compiler::parse_code(code)?;
        self.compiler.resolve_imports(&stmts, None)?;
        self.compiler.clear();
        self.compiler.symbols.add_module("__console".into());
        let mut cap = Capture::default();
        let body = Self::stmt(StmtKind::Block(self.compiler.compile_fn(&[arg_name], &mut vec![Type::Any], Self::stmt(StmtKind::Block(stmts)), &mut cap)?));
        self.compiler.tys.push(Type::Any);
        let ret_ty = self.compiler.infer_stmt(&body)?;
        self.compiler.clear();
        let fn_id = self.compile_fn(None, &[Type::Any], ret_ty.clone(), &body)?;
        self.compiler.clear();
        self.compiler.symbols.pop_module();
        self.module.finalize_definitions()?;
        Ok((self.module.get_finalized_function(fn_id) as i64, ret_ty))
    }

    pub fn import_code(&mut self, name: &str, code: Vec<u8>) -> Result<()> {
        log::info!("import {}", name);
        let _ = self.compiler.import_code(name, code)?;
        Ok(())
    }

    #[cfg(feature = "ir-disassembly")]
    pub fn disassemble_ir(&mut self, name: &str) -> Result<String> {
        if let Some(ir) = self.ir_disassembly.get(name) {
            return Ok(ir.clone());
        }
        let id = self.get_id(name)?;
        let (_, symbol) = self.compiler.symbols.get_symbol(id)?;
        if let Symbol::Fn { ty, .. } = symbol
            && let Type::Fn { tys, .. } = ty
            && tys.is_empty()
        {
            let _ = self.gen_fn(None, id, &[])?;
        }
        self.ir_disassembly.get(name).cloned().ok_or_else(|| anyhow!("未找到函数 {} 的 Cranelift IR；如果它需要参数，请先触发对应实例化", name))
    }

    pub fn get_fn_ptr(&mut self, name: &str, arg_tys: &[Type]) -> Result<(*const u8, Type)> {
        let main_id = self.get_id(name)?;
        let fn_info = self.gen_fn(None, main_id, arg_tys)?;
        Ok((self.module.get_finalized_function(fn_info.get_id()?), fn_info.get_type()?))
    }

    pub fn get_fn_ptr_with_params(&mut self, name: &str, arg_tys: &[Type], generic_args: &[Type]) -> Result<(*const u8, Type)> {
        let main_id = self.get_id(name)?;
        let fn_info = self.gen_fn_with_params(None, main_id, arg_tys, generic_args)?;
        Ok((self.module.get_finalized_function(fn_info.get_id()?), fn_info.get_type()?))
    }

    pub fn get_const_value(&mut self, ctx: &mut BuildContext, idx: usize) -> Result<(Value, Type)> {
        if self.consts.len() < idx + 1 {
            self.consts.resize(idx + 1, None);
        }
        let ptr = if let Some(ptr) = self.consts.get(idx).cloned().unwrap_or(None) {
            ptr
        } else {
            let c = Box::new(self.compiler.consts[idx].deep_clone()); //深度拷贝 避免常量被污染
            let ptr = Box::into_raw(c) as usize;
            self.consts[idx] = Some(ptr);
            ptr
        };
        let value = ctx.builder.ins().iconst(ptr_type(), ptr as i64); //需要生成副本 避免被释放
        let ty = if self.compiler.consts[idx].is_str() { Type::Str } else { Type::Any };
        Ok((self.call(ctx, self.get_method(&Type::Any, "clone")?, vec![value])?.0, ty))
    }

    fn get_null_value(&mut self, ctx: &mut BuildContext) -> Result<(Value, Type)> {
        let const_idx = self.compiler.get_const(Dynamic::Null);
        self.get_const_value(ctx, const_idx)
    }

    pub fn get_dynamic(&self, expr: &Expr) -> Option<Dynamic> {
        if let ExprKind::Const(idx) = &expr.kind { self.compiler.consts.get(*idx).cloned() } else { None }
    }

    pub fn get_method(&self, ty: &Type, name: &str) -> Result<FnInfo> {
        self.compiler.get_field(ty, name).and_then(|(_, ty)| if let Type::Symbol { id, params: _ } = ty { self.get_fn(id, &[]) } else { Err(anyhow!("不是成员函数")) })
    }

    fn is_fn_field_type(&self, ty: &Type) -> bool {
        match ty {
            Type::Symbol { id, .. } => self.compiler.symbols.get_symbol(*id).map(|(_, symbol)| symbol.is_fn()).unwrap_or(false),
            Type::Fn { .. } => true,
            _ => false,
        }
    }

    pub(crate) fn is_opaque_custom_ty(&self, ty: &Type) -> bool {
        let ty = self.compiler.symbols.get_type(ty).unwrap_or_else(|_| ty.clone());
        matches!(ty, Type::Struct { fields, .. } if !fields.is_empty() && fields.iter().all(|(_, field_ty)| self.is_fn_field_type(field_ty)))
    }

    pub(crate) fn is_aggregate_ty(&self, ty: &Type) -> bool {
        (ty.is_struct() && !self.is_opaque_custom_ty(ty)) || ty.is_array()
    }

    pub fn get_id(&self, name: &str) -> Result<u32> {
        self.compiler.symbols.get_id(name)
    }

    pub fn get_type(&mut self, name: &str, arg_tys: &[Type]) -> Result<Type> {
        let id = self.get_id(name)?;
        if self.compiler.symbols.symbols.get(name).map(|s| s.is_fn()).unwrap_or(false) {
            return self.compiler.infer_fn(id, arg_tys);
        }
        self.compiler.symbols.get_type(&Type::Symbol { id, params: Vec::new() })
    }

    pub fn new<F: FnMut(&mut JITBuilder)>(mut f: F) -> Self {
        let native_symbols = Arc::new(RwLock::new(HashMap::<String, usize>::new()));
        let lookup_symbols = native_symbols.clone();
        let mut builder = JITBuilder::new(cranelift_module::default_libcall_names()).unwrap();
        builder.symbol_lookup_fn(Box::new(move |name| lookup_symbols.read().unwrap().get(name).copied().map(|ptr| ptr as *const u8)));
        f(&mut builder);
        let module = JITModule::new(builder);
        PTR_TYPE.get_or_init(|| module.isa().pointer_type());
        let fns = BTreeMap::<u32, FnVariant>::new();
        Self {
            compiler: Compiler::new(),
            fns,
            sigs: Vec::new(),
            native_symbols,
            owner: Weak::new(),
            pending_fns: VecDeque::new(),
            compile_depth: 0,
            #[cfg(feature = "ir-disassembly")]
            ir_disassembly: BTreeMap::new(),
            module,
            consts: Vec::new(),
            scope_enter_fn: None,
            scope_exit_void_fn: None,
            scope_exit_dynamic_fn: None,
            scope_exit_bytes_fn: None,
            struct_alloc_fn: None,
            repeat_fill_fn: None,
            strcat_fn: None,
            strcat_i64_fn: None,
            strcat_assign_fn: None,
            spawn_ptr_fn: None,
            struct_from_ptr_fn: None,
            array_from_ptr_fn: None,
            array_to_ptr_fn: None,
        }
    }

    pub(crate) fn set_owner(&mut self, owner: Weak<Mutex<JITRunTime>>) {
        self.owner = owner;
    }

    pub(crate) fn owner_context_ptr(&self) -> usize {
        &self.owner as *const Weak<Mutex<JITRunTime>> as usize
    }

    fn unary(ctx: &mut BuildContext, left: (Value, Type), op: UnaryOp) -> Result<(Value, Type)> {
        match op {
            UnaryOp::Neg => {
                if left.1.is_int() || left.1.is_uint() {
                    if left.1.width() == 8 {
                        let zero = ctx.builder.ins().iconst(types::I64, 0);
                        return Ok((ctx.builder.ins().isub(zero, left.0), Type::I64));
                    } else if left.1.width() == 4 {
                        let zero = ctx.builder.ins().iconst(types::I32, 0);
                        return Ok((ctx.builder.ins().isub(zero, left.0), Type::I32));
                    }
                } else if left.1.is_float() {
                    return Ok((ctx.builder.ins().fneg(left.0), left.1));
                }
            }
            UnaryOp::Not => {
                if left.1.is_int() || left.1.is_uint() {
                    let all_ones = ctx.builder.ins().iconst(get_type(&left.1)?, -1);
                    return Ok((ctx.builder.ins().bxor(left.0, all_ones), left.1));
                }
                let zero = ctx.builder.ins().iconst(types::I8, 0);
                let one = ctx.builder.ins().iconst(types::I8, 1);
                let cond = if left.1.is_bool() {
                    left.0
                } else if left.1.is_f32() {
                    let zero = ctx.builder.ins().f32const(0.0);
                    ctx.builder.ins().fcmp(FloatCC::NotEqual, left.0, zero)
                } else if left.1.is_f64() {
                    let zero = ctx.builder.ins().f64const(0.0);
                    ctx.builder.ins().fcmp(FloatCC::NotEqual, left.0, zero)
                } else {
                    return Err(anyhow!("未实现 {:?} {:?}", left, op));
                };
                let is_zero = ctx.builder.ins().icmp_imm(IntCC::Equal, cond, 0);
                return Ok((ctx.builder.ins().select(is_zero, one, zero), Type::Bool));
            }
            _ => {}
        }
        Err(anyhow!("未实现 {:?} {:?}", left, op))
    }

    pub(crate) fn call(&mut self, ctx: &mut BuildContext, fn_info: FnInfo, args: Vec<Value>) -> Result<(Value, Type)> {
        match fn_info {
            FnInfo::Call { fn_id, arg_tys: _, caps: _, ret, context } => {
                let fn_ref = self.get_fn_ref(ctx, fn_id);
                let args = self.add_context_arg(ctx, context, args);
                let call_inst = ctx.builder.ins().call(fn_ref, &args);
                if !ret.is_void() { Ok((ctx.builder.inst_results(call_inst)[0], ret)) } else { Err(anyhow!("没有返回值")) }
            }
            FnInfo::Inline { fn_ptr, arg_tys: _ } => fn_ptr(Some(ctx), args).map(|(v, t)| (v.unwrap(), t)),
        }
    }

    pub(crate) fn scope_enter(&mut self, ctx: &mut BuildContext) -> Result<()> {
        let fn_id = self.scope_enter_fn.ok_or_else(|| anyhow!("VM scope enter runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        ctx.builder.ins().call(fn_ref, &[]);
        Ok(())
    }

    fn scope_exit_void(&mut self, ctx: &mut BuildContext) -> Result<()> {
        let fn_id = self.scope_exit_void_fn.ok_or_else(|| anyhow!("VM scope exit runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        ctx.builder.ins().call(fn_ref, &[]);
        Ok(())
    }

    fn return_value(&mut self, ctx: &mut BuildContext, value: Option<(Value, Type)>) -> Result<()> {
        let ret_ty = ctx.ret_ty.clone();
        if ret_ty.is_void() {
            self.scope_exit_void(ctx)?;
            ctx.builder.ins().return_(&[]);
            return Ok(());
        }

        let Some((value, value_ty)) = value else {
            self.scope_exit_void(ctx)?;
            ctx.builder.ins().return_(&[]);
            return Ok(());
        };

        if ret_ty.is_any() || ret_ty.is_str() || matches!(ret_ty, Type::Map | Type::List(_) | Type::Iter) {
            let value = self.convert(ctx, (value, value_ty), Type::Any)?;
            let fn_id = self.scope_exit_dynamic_fn.ok_or_else(|| anyhow!("VM dynamic return runtime is not registered"))?;
            let fn_ref = self.get_fn_ref(ctx, fn_id);
            let call_inst = ctx.builder.ins().call(fn_ref, &[value]);
            let promoted = ctx.builder.inst_results(call_inst)[0];
            ctx.builder.ins().return_(&[promoted]);
        } else if self.is_aggregate_ty(&ret_ty) {
            let value = self.convert(ctx, (value, value_ty), ret_ty.clone())?;
            let size = ctx.builder.ins().iconst(types::I64, ret_ty.width() as i64);
            let fn_id = self.scope_exit_bytes_fn.ok_or_else(|| anyhow!("VM aggregate return runtime is not registered"))?;
            let fn_ref = self.get_fn_ref(ctx, fn_id);
            let call_inst = ctx.builder.ins().call(fn_ref, &[value, size]);
            let promoted = ctx.builder.inst_results(call_inst)[0];
            ctx.builder.ins().return_(&[promoted]);
        } else {
            let value = self.convert(ctx, (value, value_ty), ret_ty)?;
            self.scope_exit_void(ctx)?;
            ctx.builder.ins().return_(&[value]);
        }
        Ok(())
    }

    fn call_for_side_effect(&mut self, ctx: &mut BuildContext, fn_info: FnInfo, args: Vec<Value>) -> Result<()> {
        match fn_info {
            FnInfo::Call { fn_id, arg_tys: _, caps: _, ret: _, context } => {
                let fn_ref = self.get_fn_ref(ctx, fn_id);
                let args = self.add_context_arg(ctx, context, args);
                ctx.builder.ins().call(fn_ref, &args);
                Ok(())
            }
            FnInfo::Inline { fn_ptr, arg_tys: _ } => fn_ptr(Some(ctx), args).map(|_| ()),
        }
    }

    fn add_context_arg(&mut self, ctx: &mut BuildContext, context: Option<usize>, mut args: Vec<Value>) -> Vec<Value> {
        if let Some(context) = context {
            let context = ctx.builder.ins().iconst(ptr_type(), context as i64);
            args.insert(0, context);
        }
        args
    }

    pub(crate) fn short_circuit_logic(&mut self, ctx: &mut BuildContext, left: (Value, Type), op: BinaryOp, right: &Expr) -> Result<(Value, Type)> {
        let left = self.bool_value(ctx, left)?;
        let rhs_block = ctx.builder.create_block();
        let short_block = ctx.builder.create_block();
        let end_block = ctx.builder.create_block();
        ctx.builder.append_block_param(end_block, types::I8);

        match op {
            BinaryOp::And => {
                ctx.builder.ins().brif(left, rhs_block, &[], short_block, &[]);
            }
            BinaryOp::Or => {
                ctx.builder.ins().brif(left, short_block, &[], rhs_block, &[]);
            }
            _ => unreachable!(),
        }

        ctx.builder.switch_to_block(rhs_block);
        let right = self.eval(ctx, right)?.get(ctx).unwrap();
        let right = self.bool_value(ctx, right)?;
        ctx.builder.ins().jump(end_block, &[cranelift::codegen::ir::BlockArg::Value(right)]);
        ctx.builder.seal_block(rhs_block);

        ctx.builder.switch_to_block(short_block);
        let short_value = match op {
            BinaryOp::And => ctx.builder.ins().iconst(types::I8, 0),
            BinaryOp::Or => ctx.builder.ins().iconst(types::I8, 1),
            _ => unreachable!(),
        };
        ctx.builder.ins().jump(end_block, &[cranelift::codegen::ir::BlockArg::Value(short_value)]);
        ctx.builder.seal_block(short_block);

        ctx.builder.switch_to_block(end_block);
        let result = ctx.builder.block_params(end_block)[0];
        Ok((result, Type::Bool))
    }

    fn struct_alloc(&mut self, ctx: &mut BuildContext, ty: &Type) -> Result<Value> {
        let size = ctx.builder.ins().iconst(types::I64, ty.width() as i64);
        let fn_id = self.struct_alloc_fn.ok_or_else(|| anyhow!("VM struct allocator runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[size]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    fn store_struct_field(&mut self, ctx: &mut BuildContext, base: Value, idx: usize, field_ty: &Type, value: (Value, Type), struct_ty: &Type) -> Result<()> {
        let offset = struct_ty.field_offset(idx).ok_or_else(|| anyhow!("结构字段索引越界 {}", idx))?;
        let value = self.convert(ctx, value, field_ty.clone())?;
        if field_ty.is_struct() || field_ty.is_array() {
            let field_addr = ctx.builder.ins().iadd_imm(base, offset as i64);
            self.copy_vec_element(ctx, field_addr, value, field_ty);
        } else {
            ctx.builder.ins().store(MemFlags::trusted(), value, base, offset as i32);
        }
        Ok(())
    }

    fn load_struct_field(&mut self, ctx: &mut BuildContext, base: Value, idx: usize, struct_ty: &Type) -> Result<(Value, Type)> {
        if let Type::Struct { params: _, fields } = struct_ty {
            let field_ty = fields.get(idx).map(|(_, ty)| ty).ok_or_else(|| anyhow!("结构字段索引越界 {}", idx))?;
            let offset = struct_ty.field_offset(idx).ok_or_else(|| anyhow!("结构字段索引越界 {}", idx))?;
            if field_ty.is_struct() || field_ty.is_array() {
                return Ok((ctx.builder.ins().iadd_imm(base, offset as i64), field_ty.clone()));
            }
            let val = ctx.builder.ins().load(crate::get_type(field_ty)?, MemFlags::trusted(), base, offset as i32);
            Ok((val, field_ty.clone()))
        } else {
            Err(anyhow!("不是结构体 {:?}", struct_ty))
        }
    }

    fn struct_field_index(&self, struct_ty: &Type, right: &Expr) -> Result<usize> {
        let value = if let ExprKind::Const(idx) = right.kind { self.compiler.consts.get(idx).cloned().ok_or_else(|| anyhow!("missing const {}", idx))? } else { right.clone().value()? };
        if let Some(idx) = value.as_int() {
            return usize::try_from(idx).map_err(|_| anyhow!("结构字段索引越界 {}", idx));
        }
        if value.is_str() {
            return self.compiler.get_field(struct_ty, value.as_str()).map(|(idx, _)| idx);
        }
        Err(anyhow!("非立即数结构字段索引 {:?}", right))
    }

    fn vec_elem_ty(ty: &Type) -> Option<Type> {
        if let Type::Vec(elem, 0) = ty { Some((**elem).clone()) } else { None }
    }

    fn array_elem_ty(ty: &Type) -> Option<Type> {
        if let Type::Array(elem, _) = ty { Some((**elem).clone()) } else { None }
    }

    fn vec_index_addr(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type) -> Result<Value> {
        let idx = self.convert(ctx, idx, Type::I64)?;
        let width = ctx.builder.ins().iconst(types::I64, elem_ty.storage_width() as i64);
        let offset = ctx.builder.ins().imul(idx, width);
        Ok(ctx.builder.ins().iadd(base, offset))
    }

    fn array_index_addr(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type) -> Result<Value> {
        self.vec_index_addr(ctx, base, idx, elem_ty)
    }

    fn load_array_index(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type) -> Result<(Value, Type)> {
        let addr = self.array_index_addr(ctx, base, idx, elem_ty)?;
        if elem_ty.is_struct() || elem_ty.is_array() {
            Ok((addr, elem_ty.clone()))
        } else {
            let val = ctx.builder.ins().load(crate::get_type(elem_ty)?, MemFlags::trusted(), addr, 0);
            Ok((val, elem_ty.clone()))
        }
    }

    fn store_array_index(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type, value: (Value, Type)) -> Result<()> {
        let addr = self.array_index_addr(ctx, base, idx, elem_ty)?;
        let value = self.convert(ctx, value, elem_ty.clone())?;
        if elem_ty.is_struct() || elem_ty.is_array() {
            self.copy_vec_element(ctx, addr, value, elem_ty);
        } else {
            let value = LocalVar::normalize_for_var(ctx, value, elem_ty);
            ctx.builder.ins().store(MemFlags::trusted(), value, addr, 0);
        }
        Ok(())
    }

    fn init_repeat_array(&mut self, ctx: &mut BuildContext, value: (Value, Type), len: u32) -> Result<(Value, Type)> {
        let elem_ty = value.1.clone();
        let array_ty = Type::Array(std::rc::Rc::new(elem_ty.clone()), len);
        let base = self.struct_alloc(ctx, &array_ty)?;
        if let Some(pattern) = self.repeat_fill_pattern(ctx, value.0, &elem_ty) {
            let fn_id = self.repeat_fill_fn.ok_or_else(|| anyhow!("VM repeat fill runtime is not registered"))?;
            let fn_ref = self.get_fn_ref(ctx, fn_id);
            let width = ctx.builder.ins().iconst(types::I64, elem_ty.storage_width() as i64);
            let len = ctx.builder.ins().iconst(types::I64, len as i64);
            ctx.builder.ins().call(fn_ref, &[base, pattern, width, len]);
            return Ok((base, array_ty));
        }
        for idx in 0..len {
            let idx = (ctx.builder.ins().iconst(types::I64, idx as i64), Type::I64);
            self.store_array_index(ctx, base, idx, &elem_ty, value.clone())?;
        }
        Ok((base, array_ty))
    }

    fn repeat_fill_pattern(&mut self, ctx: &mut BuildContext, value: Value, ty: &Type) -> Option<Value> {
        if matches!(ty, Type::Bool) || ty.is_int() || ty.is_uint() {
            return Some(if ty.storage_width() < 8 { ctx.builder.ins().uextend(types::I64, value) } else { value });
        }
        if ty.is_f32() {
            let flags = MemFlags::new().with_endianness(cranelift::codegen::ir::Endianness::Little);
            let bits = ctx.builder.ins().bitcast(types::I32, flags, value);
            return Some(ctx.builder.ins().uextend(types::I64, bits));
        }
        if ty.is_f64() {
            let flags = MemFlags::new().with_endianness(cranelift::codegen::ir::Endianness::Little);
            return Some(ctx.builder.ins().bitcast(types::I64, flags, value));
        }
        None
    }

    fn init_array_from_items(&mut self, ctx: &mut BuildContext, items: &[Expr], ty: &Type) -> Result<Value> {
        let Type::Array(elem_ty, len) = ty else {
            return Err(anyhow!("not an array type: {:?}", ty));
        };
        if items.len() != *len as usize {
            return Err(anyhow!("array literal length {} does not match {}", items.len(), len));
        }
        let base = self.struct_alloc(ctx, ty)?;
        for (idx, item) in items.iter().enumerate() {
            let value = self.eval(ctx, item)?.get(ctx).ok_or(anyhow!("array item has no value"))?;
            let idx = (ctx.builder.ins().iconst(types::I64, idx as i64), Type::I64);
            self.store_array_index(ctx, base, idx, elem_ty, value)?;
        }
        Ok(base)
    }

    pub(crate) fn any_to_array(&mut self, ctx: &mut BuildContext, value: Value, ty: &Type) -> Result<Value> {
        let Type::Array(_, _) = ty else {
            return Err(anyhow!("not an array type: {:?}", ty));
        };
        let base = self.struct_alloc(ctx, ty)?;
        let ty_ptr = Self::type_ptr_const(ctx, ty);
        let fn_id = self.array_to_ptr_fn.ok_or_else(|| anyhow!("VM array assignment runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        ctx.builder.ins().call(fn_ref, &[base, value, ty_ptr]);
        Ok(base)
    }

    fn load_vec_index(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type) -> Result<(Value, Type)> {
        let addr = self.vec_index_addr(ctx, base, idx, elem_ty)?;
        if elem_ty.is_struct() {
            Ok((addr, elem_ty.clone()))
        } else {
            let val = ctx.builder.ins().load(crate::get_type(elem_ty)?, MemFlags::trusted(), addr, 0);
            Ok((val, elem_ty.clone()))
        }
    }

    fn copy_vec_element(&mut self, ctx: &mut BuildContext, dst: Value, src: Value, elem_ty: &Type) {
        let mut offset = 0u32;
        let width = elem_ty.storage_width();
        while offset < width {
            let remaining = width - offset;
            let (ty, size) = if remaining >= 8 {
                (types::I64, 8)
            } else if remaining >= 4 {
                (types::I32, 4)
            } else if remaining >= 2 {
                (types::I16, 2)
            } else {
                (types::I8, 1)
            };
            let value = ctx.builder.ins().load(ty, MemFlags::trusted(), src, offset as i32);
            ctx.builder.ins().store(MemFlags::trusted(), value, dst, offset as i32);
            offset += size;
        }
    }

    fn store_vec_index(&mut self, ctx: &mut BuildContext, base: Value, idx: (Value, Type), elem_ty: &Type, value: (Value, Type)) -> Result<()> {
        let addr = self.vec_index_addr(ctx, base, idx, elem_ty)?;
        let value = self.convert(ctx, value, elem_ty.clone())?;
        if elem_ty.is_struct() {
            self.copy_vec_element(ctx, addr, value, elem_ty);
        } else {
            let value = LocalVar::normalize_for_var(ctx, value, elem_ty);
            ctx.builder.ins().store(MemFlags::trusted(), value, addr, 0);
        }
        Ok(())
    }

    fn swap_vec_index(&mut self, ctx: &mut BuildContext, base: Value, left: (Value, Type), right: (Value, Type), elem_ty: &Type) -> Result<()> {
        let left_addr = self.vec_index_addr(ctx, base, left, elem_ty)?;
        let right_addr = self.vec_index_addr(ctx, base, right, elem_ty)?;
        let mut offset = 0u32;
        let width = elem_ty.storage_width();
        while offset < width {
            let remaining = width - offset;
            let (ty, size) = if remaining >= 8 {
                (types::I64, 8)
            } else if remaining >= 4 {
                (types::I32, 4)
            } else if remaining >= 2 {
                (types::I16, 2)
            } else {
                (types::I8, 1)
            };
            let left_value = ctx.builder.ins().load(ty, MemFlags::trusted(), left_addr, offset as i32);
            let right_value = ctx.builder.ins().load(ty, MemFlags::trusted(), right_addr, offset as i32);
            ctx.builder.ins().store(MemFlags::trusted(), left_value, right_addr, offset as i32);
            ctx.builder.ins().store(MemFlags::trusted(), right_value, left_addr, offset as i32);
            offset += size;
        }
        Ok(())
    }

    fn init_struct_from_dynamic(&mut self, ctx: &mut BuildContext, value: (Value, Type), ty: &Type) -> Result<Value> {
        let Type::Struct { params: _, fields } = ty else {
            return Err(anyhow!("不是结构体 {:?}", ty));
        };
        let base = self.struct_alloc(ctx, ty)?;
        for (idx, (_, field_ty)) in fields.iter().enumerate() {
            let idx_val = ctx.builder.ins().iconst(types::I64, idx as i64);
            let item = self.call(ctx, self.get_method(&Type::Any, "get_idx")?, vec![value.0, idx_val])?;
            self.store_struct_field(ctx, base, idx, field_ty, item, ty)?;
        }
        Ok(base)
    }

    fn init_struct_from_items(&mut self, ctx: &mut BuildContext, items: &[Expr], ty: &Type) -> Result<Value> {
        let Type::Struct { params: _, fields } = ty else {
            return Err(anyhow!("not a struct type: {:?}", ty));
        };
        let base = self.struct_alloc(ctx, ty)?;
        for (idx, item) in items.iter().enumerate() {
            let Some((_, field_ty)) = fields.get(idx) else {
                break;
            };
            let value = self.eval(ctx, item)?.get(ctx).ok_or(anyhow!("struct field has no value"))?;
            self.store_struct_field(ctx, base, idx, field_ty, value, ty)?;
        }
        Ok(base)
    }

    fn expr_assigned_var(expr: &Expr) -> Option<(u32, Type)> {
        if let ExprKind::Binary { left, op, right } = &expr.kind
            && op.is_assign()
            && let ExprKind::Var(idx) = left.kind
        {
            return Some((idx, right.get_type()));
        }
        None
    }

    fn declare_assigned_vars(&mut self, ctx: &mut BuildContext, stmt: &Stmt) -> Result<()> {
        match &stmt.kind {
            StmtKind::Expr(expr, _) => {
                if let Some((idx, ty)) = Self::expr_assigned_var(expr) {
                    match ctx.get_var(idx).ok() {
                        Some(LocalVar::Variable { .. }) | Some(LocalVar::Closure { .. }) => {}
                        Some(LocalVar::Value { val, ty }) => {
                            ctx.set_var(idx, LocalVar::Value { val, ty })?;
                        }
                        Some(LocalVar::None) | None => {
                            let init = self.zero_value(ctx, &ty)?;
                            ctx.set_var(idx, init.into())?;
                        }
                    }
                }
            }
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.declare_assigned_vars(ctx, stmt)?;
                }
            }
            StmtKind::If { then_body, else_body, .. } => {
                self.declare_assigned_vars(ctx, then_body)?;
                if let Some(else_body) = else_body {
                    self.declare_assigned_vars(ctx, else_body)?;
                }
            }
            StmtKind::While { body, .. } | StmtKind::Loop(body) => {
                self.declare_assigned_vars(ctx, body)?;
            }
            StmtKind::For { body, .. } => {
                self.declare_assigned_vars(ctx, body)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn zero_value(&mut self, ctx: &mut BuildContext, ty: &Type) -> Result<(Value, Type)> {
        if self.is_aggregate_ty(ty) {
            Ok((self.struct_alloc(ctx, ty)?, ty.clone()))
        } else if ty.is_f32() {
            Ok((ctx.builder.ins().f32const(0.0), ty.clone()))
        } else if ty.is_f64() {
            Ok((ctx.builder.ins().f64const(0.0), ty.clone()))
        } else {
            Ok((ctx.builder.ins().iconst(crate::get_type(ty)?, 0), ty.clone()))
        }
    }

    fn assign(&mut self, ctx: &mut BuildContext, left: &Expr, value: LocalVar) -> Result<(Value, Type)> {
        if let ExprKind::Var(idx) = &left.kind {
            if value.is_closure() {
                ctx.set_var(*idx, value)?;
                return self.get_null_value(ctx);
            }
            let value_ty = value.get_ty();
            if let Some(ty) = ctx.get_var_ty(*idx) {
                if self.is_aggregate_ty(&ty) {
                    let dst = ctx.get_var(*idx)?.get(ctx).ok_or(anyhow!("aggregate variable has no value"))?.0;
                    let src = value.get(ctx).ok_or(anyhow!("aggregate assignment has no value"))?;
                    let src = self.convert(ctx, src, ty.clone())?;
                    self.copy_vec_element(ctx, dst, src, &ty);
                } else if value_ty != ty {
                    if let Some(vt) = value.get(ctx) {
                        let val = self.convert(ctx, vt, ty.clone())?;
                        ctx.set_var(*idx, LocalVar::Value { val, ty })?;
                    } else if ty.is_any() {
                        let const_idx = self.compiler.get_const(Dynamic::Null);
                        let (val, ty) = self.get_const_value(ctx, const_idx)?;
                        ctx.set_var(*idx, LocalVar::Value { val, ty })?;
                    } else {
                        ctx.set_var(*idx, LocalVar::None)?;
                    }
                } else {
                    ctx.set_var(*idx, value)?;
                }
            } else if self.is_aggregate_ty(&value_ty) {
                let src = value.get(ctx).ok_or(anyhow!("aggregate initializer has no value"))?;
                let dst = self.struct_alloc(ctx, &value_ty)?;
                let src = self.convert(ctx, src, value_ty.clone())?;
                self.copy_vec_element(ctx, dst, src, &value_ty);
                ctx.set_var(*idx, LocalVar::Value { val: dst, ty: value_ty })?;
            } else {
                ctx.set_var(*idx, value)?;
            }
            let assigned = ctx.get_var(*idx)?;
            if assigned.is_closure() {
                return self.get_null_value(ctx);
            }
            let val = assigned.get(ctx).ok_or(anyhow!("assigned variable has no value"))?;
            return Ok(val);
        } else if left.is_idx() {
            let value = value.get(ctx).unwrap();
            let (left, _, right) = left.clone().binary().unwrap();
            let left = self.eval(ctx, &left)?.get(ctx).ok_or(anyhow!("未知局部变量 {:?}", left))?;
            if let Type::Struct { params: _, fields } = &left.1 {
                let idx = self.struct_field_index(&left.1, &right)?;
                let field_ty = fields.get(idx).map(|(_, ty)| ty.clone()).ok_or_else(|| anyhow!("结构字段索引越界 {}", idx))?;
                self.store_struct_field(ctx, left.0, idx, &field_ty, value.clone(), &left.1)?;
                return Ok(value);
            }
            if let Some(elem_ty) = Self::vec_elem_ty(&left.1) {
                let idx = if right.is_value() {
                    let idx = right.clone().value()?.as_int().ok_or(anyhow!("Vec 索引必须是整数"))?;
                    (ctx.builder.ins().iconst(types::I64, idx), Type::I64)
                } else {
                    self.eval(ctx, &right)?.get(ctx).ok_or(anyhow!("Vec 索引没有值"))?
                };
                self.store_vec_index(ctx, left.0, idx, &elem_ty, value.clone())?;
                return Ok(value);
            }
            if let Some(elem_ty) = Self::array_elem_ty(&left.1) {
                let idx = if right.is_value() {
                    let idx = right.clone().value()?.as_int().ok_or(anyhow!("array index must be integer"))?;
                    (ctx.builder.ins().iconst(types::I64, idx), Type::I64)
                } else {
                    self.eval(ctx, &right)?.get(ctx).ok_or(anyhow!("array index has no value"))?
                };
                self.store_array_index(ctx, left.0, idx, &elem_ty, value.clone())?;
                return Ok(value);
            }
            if right.is_value() {
                let right_value = right.clone().value()?;
                if let Some(idx) = right_value.as_int() {
                    let idx = ctx.builder.ins().iconst(types::I64, idx);
                    if let Type::List(elem_ty) = &left.1
                        && let Some((fn_name, value_ty)) = Self::list_set_idx_shortcut(elem_ty)
                    {
                        let stored = self.convert(ctx, value.clone(), value_ty.clone())?;
                        let set_idx_fn = self.get_fn(self.get_id(fn_name)?, &[Type::Any, Type::I64, value_ty])?;
                        self.call_for_side_effect(ctx, set_idx_fn, vec![left.0, idx, stored])?;
                        return Ok(value);
                    }
                    let f = self.get_method(&left.1, "set_idx")?;
                    let args = self.adjust_args(ctx, vec![left, (idx, Type::I64), value.clone()], f.arg_tys()?)?;
                    self.call_for_side_effect(ctx, f, args)?;
                } else {
                    let key = ctx.get_const(&right_value)?;
                    let f = self.get_method(&left.1, "set_key")?;
                    let args = self.adjust_args(ctx, vec![left, key, value.clone()], f.arg_tys()?)?;
                    self.call_for_side_effect(ctx, f, args)?;
                }
            } else {
                let right = self.eval(ctx, &right)?.get(ctx).unwrap();
                if right.1.is_any() || right.1.is_str() {
                    let f = self.get_method(&left.1, "set_key")?;
                    let args = self.adjust_args(ctx, vec![left, right, value.clone()], f.arg_tys()?)?;
                    self.call_for_side_effect(ctx, f, args)?;
                } else {
                    if let Type::List(elem_ty) = &left.1
                        && let Some((fn_name, value_ty)) = Self::list_set_idx_shortcut(elem_ty)
                    {
                        let idx = self.convert(ctx, right.clone(), Type::I64)?;
                        let stored = self.convert(ctx, value.clone(), value_ty.clone())?;
                        let set_idx_fn = self.get_fn(self.get_id(fn_name)?, &[Type::Any, Type::I64, value_ty])?;
                        self.call_for_side_effect(ctx, set_idx_fn, vec![left.0, idx, stored])?;
                        return Ok(value);
                    }
                    let f = self.get_method(&left.1, "set_idx")?;
                    let args = self.adjust_args(ctx, vec![left, right, value.clone()], f.arg_tys()?)?;
                    self.call_for_side_effect(ctx, f, args)?;
                }
            }
            return Ok(value);
        } else {
            panic!("赋值给 {:?} {:?}", left, value)
        }
    }

    fn assignment_target_ty(&mut self, ctx: &mut BuildContext, left: &Expr) -> Option<Type> {
        if let ExprKind::Var(idx) = &left.kind {
            return ctx.get_var_ty(*idx).filter(|ty| !ty.is_any()).or_else(|| ctx.local_type_hint(*idx));
        }
        None
    }

    fn empty_typed_list(ty: &Type) -> Option<Dynamic> {
        let Type::List(elem_ty) = ty else {
            return None;
        };
        match elem_ty.as_ref() {
            Type::Bool | Type::U8 => Some(Dynamic::list(Vec::new())),
            Type::I8 => Some(Dynamic::VecI8(Default::default())),
            Type::U16 => Some(Dynamic::VecU16(Default::default())),
            Type::I16 => Some(Dynamic::VecI16(Default::default())),
            Type::U32 => Some(Dynamic::VecU32(Default::default())),
            Type::I32 => Some(Dynamic::VecI32(Default::default())),
            Type::F32 => Some(Dynamic::VecF32(Default::default())),
            Type::U64 => Some(Dynamic::VecU64(Vec::new())),
            Type::I64 => Some(Dynamic::VecI64(Vec::new())),
            Type::F64 => Some(Dynamic::VecF64(Vec::new())),
            Type::Str => Some(Dynamic::list(Vec::new())),
            _ => None,
        }
    }

    fn list_push_shortcut(elem_ty: &Type) -> Option<(&'static str, Type)> {
        match elem_ty {
            Type::Bool => Some(("Any::push_bool", Type::Bool)),
            Type::U8 => Some(("Any::push_u8", Type::U8)),
            Type::I8 => Some(("Any::push_i8", Type::I8)),
            Type::U16 => Some(("Any::push_u16", Type::U16)),
            Type::I16 => Some(("Any::push_i16", Type::I16)),
            Type::U32 => Some(("Any::push_u32", Type::U32)),
            Type::I32 => Some(("Any::push_i32", Type::I32)),
            Type::F32 => Some(("Any::push_f32", Type::F32)),
            Type::U64 => Some(("Any::push_u64", Type::U64)),
            Type::I64 => Some(("Any::push_i64", Type::I64)),
            Type::F64 => Some(("Any::push_f64", Type::F64)),
            Type::Str => Some(("Any::push_str", Type::Str)),
            _ => None,
        }
    }

    fn list_get_idx_shortcut(elem_ty: &Type) -> Option<(&'static str, Type)> {
        match elem_ty {
            Type::Bool => Some(("Any::get_idx_bool", Type::Bool)),
            Type::U8 => Some(("Any::get_idx_u8", Type::U8)),
            Type::I8 => Some(("Any::get_idx_i8", Type::I8)),
            Type::U16 => Some(("Any::get_idx_u16", Type::U16)),
            Type::I16 => Some(("Any::get_idx_i16", Type::I16)),
            Type::U32 => Some(("Any::get_idx_u32", Type::U32)),
            Type::I32 => Some(("Any::get_idx_i32", Type::I32)),
            Type::F32 => Some(("Any::get_idx_f32", Type::F32)),
            Type::U64 => Some(("Any::get_idx_u64", Type::U64)),
            Type::I64 => Some(("Any::get_idx_i64", Type::I64)),
            Type::F64 => Some(("Any::get_idx_f64", Type::F64)),
            Type::Str => Some(("Any::get_idx_str", Type::Str)),
            _ => None,
        }
    }

    fn list_set_idx_shortcut(elem_ty: &Type) -> Option<(&'static str, Type)> {
        match elem_ty {
            Type::Bool => Some(("Any::set_idx_bool", Type::Bool)),
            Type::U8 => Some(("Any::set_idx_u8", Type::U8)),
            Type::I8 => Some(("Any::set_idx_i8", Type::I8)),
            Type::U16 => Some(("Any::set_idx_u16", Type::U16)),
            Type::I16 => Some(("Any::set_idx_i16", Type::I16)),
            Type::U32 => Some(("Any::set_idx_u32", Type::U32)),
            Type::I32 => Some(("Any::set_idx_i32", Type::I32)),
            Type::F32 => Some(("Any::set_idx_f32", Type::F32)),
            Type::U64 => Some(("Any::set_idx_u64", Type::U64)),
            Type::I64 => Some(("Any::set_idx_i64", Type::I64)),
            Type::F64 => Some(("Any::set_idx_f64", Type::F64)),
            Type::Str => Some(("Any::set_idx_str", Type::Str)),
            _ => None,
        }
    }

    fn expr_is_empty_list(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Value(value) => value.is_list() && value.len() == 0,
            ExprKind::Const(idx) => self.compiler.consts.get(*idx).is_some_and(|value| value.is_list() && value.len() == 0),
            ExprKind::Typed { value, .. } => self.expr_is_empty_list(value),
            _ => false,
        }
    }

    fn closure_value(&self, ctx: &mut BuildContext, id: u32) -> Result<LocalVar> {
        let captures = match self.compiler.symbols.get_symbol(id)?.1 {
            Symbol::Fn { cap, .. } => cap.vars.iter().map(|idx| ctx.get_var(*idx as u32)?.get(ctx).ok_or_else(|| anyhow!("捕获变量 {} 没有值", idx))).collect::<Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Ok(LocalVar::Closure { id, captures })
    }

    fn is_spawn_fn_name(name: &str) -> bool {
        name == "spawn" || name == "std::spawn"
    }

    fn spawn_arg_pack_len(&self, expr: &Expr) -> Option<usize> {
        match &expr.kind {
            ExprKind::Tuple(items) | ExprKind::List(items) => Some(items.len()),
            ExprKind::Value(value) => value.is_list().then(|| value.len()),
            ExprKind::Const(idx) => self.compiler.consts.get(*idx).and_then(|value| value.is_list().then(|| value.len())),
            ExprKind::Typed { value, .. } => self.spawn_arg_pack_len(value),
            _ => None,
        }
    }

    fn eval_spawn_arg_pack(&mut self, ctx: &mut BuildContext, expr: &Expr) -> Result<(Value, Type)> {
        let (ExprKind::Tuple(items) | ExprKind::List(items)) = &expr.kind else {
            return self.eval(ctx, expr)?.get(ctx).ok_or_else(|| anyhow!("spawn closure args expression has no value"));
        };
        let idx = self.compiler.get_const(Dynamic::list(vec![Dynamic::Null; items.len()]));
        let (list, _) = self.get_const_value(ctx, idx)?;
        for (idx, item) in items.iter().enumerate() {
            let value = self.eval(ctx, item)?.get(ctx).ok_or_else(|| anyhow!("spawn closure arg has no value: {:?}", item))?;
            let value = self.convert(ctx, value, Type::Any)?;
            let idx = ctx.builder.ins().iconst(types::I64, idx as i64);
            let set_idx = self.get_fn(self.get_id("Any::set_idx")?, &[Type::Any, Type::I64, Type::Any])?;
            self.call_for_side_effect(ctx, set_idx, vec![list, idx, value])?;
        }
        Ok((list, Type::Any))
    }

    fn spawn_closure(&mut self, ctx: &mut BuildContext, id: u32, captures: Vec<(Value, Type)>, args_expr: &Expr) -> Result<LocalVar> {
        if !captures.is_empty() {
            return Err(anyhow!("spawn closure does not support captures yet"));
        }
        let arg_len = self.spawn_arg_pack_len(args_expr).ok_or_else(|| anyhow!("spawn closure args must be a tuple argument pack"))?;
        if arg_len > 8 {
            return Err(anyhow!("spawn supports at most 8 args, got {}", arg_len));
        }
        let arg_tys = vec![Type::Any; arg_len];
        let fn_info = self.gen_fn_with_params(Some(ctx), id, &arg_tys, &[])?;
        let FnInfo::Call { fn_id, ret, .. } = fn_info else {
            return Err(anyhow!("spawn closure target must be compiled function"));
        };
        let args = self.eval_spawn_arg_pack(ctx, args_expr)?;
        let args = self.convert(ctx, args, Type::Any)?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let fn_addr = ctx.builder.ins().func_addr(ptr_type(), fn_ref);
        let ret_ty = Self::type_ptr_const(ctx, &ret);
        let spawn_ptr = self.spawn_ptr_fn.ok_or_else(|| anyhow!("VM spawn ptr runtime is not registered"))?;
        let spawn_ref = self.get_fn_ref(ctx, spawn_ptr);
        let call_inst = ctx.builder.ins().call(spawn_ref, &[fn_addr, ret_ty, args]);
        Ok((ctx.builder.inst_results(call_inst)[0], Type::Bool).into())
    }

    pub(crate) fn call_fn(&mut self, ctx: &mut BuildContext, id: u32, obj: Option<Expr>, params: &Vec<Expr>) -> Result<LocalVar> {
        self.call_fn_with_params(ctx, id, &[], obj, params)
    }

    pub(crate) fn call_fn_with_params(&mut self, ctx: &mut BuildContext, id: u32, generic_args: &[Type], obj: Option<Expr>, params: &Vec<Expr>) -> Result<LocalVar> {
        self.call_fn_with_capture_values(ctx, id, generic_args, obj, params, None)
    }

    pub(crate) fn call_fn_with_capture_values(&mut self, ctx: &mut BuildContext, id: u32, generic_args: &[Type], obj: Option<Expr>, params: &Vec<Expr>, capture_values: Option<Vec<(Value, Type)>>) -> Result<LocalVar> {
        let fn_name = self.compiler.symbols.get_symbol(id).map(|(name, _)| name.clone())?;
        if capture_values.is_none()
            && generic_args.is_empty()
            && obj.is_none()
            && Self::is_spawn_fn_name(fn_name.as_str())
            && let [target, args] = params.as_slice()
            && let LocalVar::Closure { id, captures } = self.eval(ctx, target)?
        {
            return self.spawn_closure(ctx, id, captures, args);
        }
        let mut args: Vec<(Value, Type)> = if let Some(obj) = obj { vec![self.eval(ctx, &obj)?.get(ctx).ok_or_else(|| anyhow!("函数 {} 的接收者表达式没有值: {:?}", fn_name, obj))?] } else { Vec::new() };
        for p in params {
            args.push(self.eval(ctx, p)?.get(ctx).ok_or_else(|| anyhow!("函数 {} 的参数表达式没有值: {:?}", fn_name, p))?);
        }
        if let Some(captures) = &capture_values {
            args.extend(captures.iter().cloned());
        }
        if let [list, value] = args.as_slice()
            && fn_name.as_str() == "Any::push"
            && let Type::List(elem_ty) = &list.1
            && let Some((fn_name, value_ty)) = Self::list_push_shortcut(elem_ty)
        {
            let value = self.convert(ctx, (value.0, value.1.clone()), value_ty.clone())?;
            let push_fn = self.get_fn(self.get_id(fn_name)?, &[Type::Any, value_ty])?;
            self.call_for_side_effect(ctx, push_fn, vec![list.0, value])?;
            return Ok(LocalVar::None);
        }
        if let [list, idx] = args.as_slice()
            && fn_name.as_str() == "Any::get_idx"
            && let Type::List(elem_ty) = &list.1
            && let Some((fn_name, _ret_ty)) = Self::list_get_idx_shortcut(elem_ty)
        {
            let idx = self.convert(ctx, (idx.0, idx.1.clone()), Type::I64)?;
            let get_idx_fn = self.get_fn(self.get_id(fn_name)?, &[Type::Any, Type::I64])?;
            return self.call(ctx, get_idx_fn, vec![list.0, idx]).map(|value| value.into());
        }
        if fn_name.as_str().ends_with("Vec::swap")
            && let Some((base, vec_ty)) = args.first().cloned()
            && let Some(elem_ty) = Self::vec_elem_ty(&vec_ty)
        {
            let [_, left_idx, right_idx]: [(Value, Type); 3] = args.try_into().map_err(|_| anyhow!("Vec::swap 需要 self 和两个索引参数"))?;
            self.swap_vec_index(ctx, base, left_idx, right_idx, &elem_ty)?;
            return Ok(LocalVar::None);
        }
        let visible_arg_len = args.len() - capture_values.as_ref().map(|captures| captures.len()).unwrap_or(0);
        let arg_tys: Vec<Type> = args.iter().take(visible_arg_len).map(|(_, ty)| ty.clone()).collect();
        let fn_info = match if generic_args.is_empty() { self.get_fn(id, &arg_tys) } else { Err(anyhow!("generic function needs specialization")) } {
            Ok(info) => info,
            Err(_) => self.gen_fn_with_params(Some(ctx), id, &arg_tys, generic_args).map_err(|e| {
                log::error!("{:?}", self.compiler.symbols.get_symbol(id));
                e
            })?,
        };
        match &fn_info {
            FnInfo::Call { fn_id: _, arg_tys: want_tys, caps, ret, context: _ } => {
                let mut args = self.adjust_args(ctx, args, want_tys)?;
                if capture_values.is_none() {
                    for c in caps {
                        args.push(ctx.get_var(*c as u32)?.get(ctx).unwrap().0);
                    }
                }
                if ret.is_void() {
                    self.call_for_side_effect(ctx, fn_info, args)?;
                    Ok(LocalVar::None)
                } else {
                    self.call(ctx, fn_info, args).map(|r| r.into())
                }
            }
            _ => panic!("不可能编译出 inline 函数"),
        }
    }

    pub(crate) fn eval(&mut self, ctx: &mut BuildContext, expr: &Expr) -> Result<LocalVar> {
        self.eval_with_expected(ctx, expr, None)
    }

    fn eval_with_expected(&mut self, ctx: &mut BuildContext, expr: &Expr, expected: Option<&Type>) -> Result<LocalVar> {
        if let Some(ty) = expected
            && self.expr_is_empty_list(expr)
            && let Some(value) = Self::empty_typed_list(ty)
        {
            let idx = self.compiler.get_const(value);
            let (val, _) = self.get_const_value(ctx, idx)?;
            return Ok(LocalVar::Value { val, ty: ty.clone() });
        }
        match &expr.kind {
            ExprKind::Value(v) => Ok(ctx.get_const(v)?.into()),
            ExprKind::Var(idx) => {
                let v = ctx.get_var(*idx)?;
                Ok(v)
            }
            ExprKind::Unary { op, value } => {
                let v = self.eval(ctx, value)?.get(ctx).unwrap();
                if op == &UnaryOp::Not && v.1.is_any() {
                    let cond = self.bool_value(ctx, v)?;
                    let zero = ctx.builder.ins().iconst(types::I8, 0);
                    let one = ctx.builder.ins().iconst(types::I8, 1);
                    let is_zero = ctx.builder.ins().icmp_imm(IntCC::Equal, cond, 0);
                    Ok((ctx.builder.ins().select(is_zero, one, zero), Type::Bool).into())
                } else {
                    Ok(Self::unary(ctx, v, op.clone())?.into())
                }
            }
            ExprKind::Binary { left, op, right } => {
                if op == &BinaryOp::Assign {
                    let expected = self.assignment_target_ty(ctx, left);
                    match self.eval_with_expected(ctx, right, expected.as_ref()) {
                        Ok(value) => self.assign(ctx, left, value).map(|v| v.into()),
                        Err(e) => {
                            log::error!("assign error {:?}", e);
                            Err(e)
                        }
                    }
                } else {
                    let assign_expr = if op.is_assign() { Some(left.clone()) } else { None };
                    let assign_expected = if op.is_assign() { self.assignment_target_ty(ctx, left) } else { None };
                    let left = match self.eval(ctx, left)?.get(ctx) {
                        Some(left) => left,
                        None => return Err(anyhow!("binary left has no value: {:?}", left)),
                    };
                    if op == &BinaryOp::Idx {
                        let left_ty = self.compiler.symbols.get_type(&left.1).unwrap_or_else(|_| left.1.clone());
                        let left = (left.0, left_ty);
                        if let Type::Struct { params: _, fields: _ } = &left.1 {
                            let idx = self.struct_field_index(&left.1, right)?;
                            return self.load_struct_field(ctx, left.0, idx, &left.1).map(|r| r.into());
                        }
                        if let Some(elem_ty) = Self::vec_elem_ty(&left.1) {
                            let idx = if right.is_value() {
                                let idx = right.clone().value()?.as_int().ok_or(anyhow!("Vec 索引必须是整数"))?;
                                (ctx.builder.ins().iconst(types::I64, idx), Type::I64)
                            } else {
                                self.eval(ctx, right)?.get(ctx).ok_or(anyhow!("Vec 索引没有值"))?
                            };
                            return self.load_vec_index(ctx, left.0, idx, &elem_ty).map(|r| r.into());
                        }
                        if let Some(elem_ty) = Self::array_elem_ty(&left.1) {
                            let idx = if right.is_value() {
                                let idx = right.clone().value()?.as_int().ok_or(anyhow!("array index must be integer"))?;
                                (ctx.builder.ins().iconst(types::I64, idx), Type::I64)
                            } else {
                                self.eval(ctx, right)?.get(ctx).ok_or(anyhow!("array index has no value"))?
                            };
                            return self.load_array_index(ctx, left.0, idx, &elem_ty).map(|r| r.into());
                        }
                        if right.is_value() {
                            let right_value = right.clone().value()?;
                            if let Some(idx) = right_value.as_int() {
                                let idx = ctx.builder.ins().iconst(types::I64, idx);
                                self.call(ctx, self.get_method(&left.1, "get_idx")?, vec![left.0, idx]).map(|r| r.into())
                            } else {
                                let key = ctx.get_const(&right_value)?;
                                self.call(ctx, self.get_method(&left.1, "get_key")?, vec![left.0, key.0]).map(|r| r.into())
                            }
                        } else if let ExprKind::Range { start, stop, inclusive } = &right.kind {
                            let start = self.eval(ctx, start)?.get(ctx).ok_or(anyhow!("range start has no value"))?;
                            let start = self.convert(ctx, start, Type::I64)?;
                            let stop = self.eval(ctx, stop)?.get(ctx).ok_or(anyhow!("range stop has no value"))?;
                            let stop = self.convert(ctx, stop, Type::Any)?;
                            let inclusive = ctx.builder.ins().iconst(types::I8, i64::from(*inclusive));
                            self.call(ctx, self.get_method(&left.1, "slice")?, vec![left.0, start, stop, inclusive]).map(|r| r.into())
                        } else {
                            let right = self.eval(ctx, right)?.get(ctx).ok_or(anyhow!("非Value {:?}", right))?;
                            if right.1.is_any() || right.1.is_str() {
                                let right = self.convert(ctx, right, Type::Any)?;
                                self.call(ctx, self.get_method(&left.1, "get_key")?, vec![left.0, right]).map(|r| r.into())
                            } else {
                                let right = self.convert(ctx, right, Type::I64)?;
                                if let Type::List(elem_ty) = &left.1
                                    && let Some((fn_name, _ret_ty)) = Self::list_get_idx_shortcut(elem_ty)
                                {
                                    let get_idx_fn = self.get_fn(self.get_id(fn_name)?, &[Type::Any, Type::I64])?;
                                    return self.call(ctx, get_idx_fn, vec![left.0, right]).map(|r| r.into());
                                }
                                self.call(ctx, self.get_method(&left.1, "get_idx")?, vec![left.0, right]).map(|r| r.into())
                            }
                        }
                    } else {
                        let result = self.binary_with_expected(ctx, left, op.clone(), right, assign_expected.as_ref().or(expected))?.into();
                        if let Some(expr) = assign_expr { self.assign(ctx, &expr, result).map(|r| r.into()) } else { Ok(result.into()) }
                    }
                }
            }
            ExprKind::Call { obj, params } => {
                if let ExprKind::AssocId { id, params: generic_args } = &obj.kind {
                    self.call_fn_with_params(ctx, *id, generic_args, None, params)
                } else if let ExprKind::Id(id, obj) = &obj.kind {
                    self.call_fn(ctx, *id, obj.as_ref().map(|o| *o.clone()), params)
                } else if obj.is_value() {
                    //直接忽略掉的代码 编译期就可以忽略
                    return Ok(LocalVar::None);
                } else {
                    if obj.is_idx() {
                        let (left, _, right) = obj.clone().binary().unwrap();
                        let left = self.eval(ctx, &left)?.get(ctx).ok_or(anyhow!("obj {:?}", obj))?;
                        let ty = self.compiler.symbols.get_type(&left.1)?;
                        if let Some(name) = self.get_dynamic(&right) {
                            if name.as_str() == "swap"
                                && let Some(elem_ty) = Self::vec_elem_ty(&ty)
                            {
                                let [left_idx, right_idx]: [(Value, Type); 2] =
                                    params.iter().map(|p| self.eval(ctx, p)?.get(ctx).ok_or(anyhow!("Vec::swap 参数没有值"))).collect::<Result<Vec<_>>>()?.try_into().map_err(|_| anyhow!("Vec::swap 需要两个索引参数"))?;
                                self.swap_vec_index(ctx, left.0, left_idx, right_idx, &elem_ty)?;
                                return Ok(LocalVar::None);
                            }
                            let mut args = vec![left];
                            for p in params {
                                args.push(self.eval(ctx, p)?.get(ctx).ok_or_else(|| anyhow!("动态方法 {:?} 的参数表达式没有值: {:?}", name, p))?);
                            }
                            let (_, method_ty) = self.compiler.get_field(&ty, name.as_str())?;
                            let Type::Symbol { id, .. } = method_ty else {
                                return Err(anyhow!("不是成员函数"));
                            };
                            let arg_tys: Vec<Type> = args.iter().map(|(_, ty)| ty.clone()).collect();
                            let method = self.get_fn(id, &arg_tys).or_else(|_| self.gen_fn_with_params(Some(ctx), id, &arg_tys, &[]))?;
                            let args = self.adjust_args(ctx, args, method.arg_tys()?)?;
                            self.call(ctx, method, args).map(|r| r.into())
                        } else {
                            self.eval(ctx, obj)
                        }
                    } else {
                        let val = self.eval(ctx, obj)?;
                        if let LocalVar::Closure { id, captures } = val {
                            return self.call_fn_with_capture_values(ctx, id, &[], None, params, Some(captures));
                        }
                        panic!("暂未实现 {:?}", val)
                    }
                }
            }
            ExprKind::Typed { value, ty } => {
                if let Type::Struct { params: _, fields: _ } = ty
                    && let ExprKind::List(items) = &value.kind
                {
                    return Ok((self.init_struct_from_items(ctx, items, ty)?, ty.clone()).into());
                }
                if let Type::Array(_, _) = ty
                    && let ExprKind::List(items) = &value.kind
                {
                    return Ok((self.init_array_from_items(ctx, items, ty)?, ty.clone()).into());
                }
                let evaluated = self.eval(ctx, value)?;
                if evaluated.is_closure() {
                    return Ok(evaluated);
                }
                let vt = if let Some(vt) = evaluated.get(ctx) {
                    vt
                } else if ty.is_any() {
                    let idx = self.compiler.get_const(Dynamic::Null);
                    self.get_const_value(ctx, idx)?
                } else {
                    return Ok(LocalVar::None);
                };
                if let Type::Struct { params: _, fields: _ } = ty
                    && !self.is_opaque_custom_ty(ty)
                {
                    if &vt.1 == ty {
                        Ok(vt.into())
                    } else if vt.1.is_any() {
                        Ok((self.init_struct_from_dynamic(ctx, vt, ty)?, ty.clone()).into())
                    } else {
                        Err(anyhow!("cannot convert {:?} to {:?}", vt.1, ty))
                    }
                } else if &vt.1 != ty {
                    Ok((self.convert(ctx, vt, ty.clone())?, ty.clone()).into())
                } else {
                    Ok(vt.into())
                }
            }
            ExprKind::List(_) => Err(anyhow!("未实现 {:?}", expr)),
            ExprKind::Repeat { value, len } => {
                let value = self.eval(ctx, value)?.get(ctx).ok_or(anyhow!("repeat value has no value"))?;
                let Type::ConstInt(len) = len else {
                    return Err(anyhow!("repeat length must be a compile-time integer"));
                };
                let len = u32::try_from(*len).map_err(|_| anyhow!("repeat length out of range"))?;
                self.init_repeat_array(ctx, value, len).map(|r| r.into())
            }
            ExprKind::Const(idx) => self.get_const_value(ctx, *idx).map(|v| v.into()),
            ExprKind::Id(id, _) => self.closure_value(ctx, *id),
            ExprKind::AssocId { id, .. } => self.closure_value(ctx, *id),
            expr => {
                //结构就是一块固定大小 的内存(或者是动态大小 最后一个数据成员可扩展 跟 C 结构一样)
                panic!("未实现 {:?}", expr)
            }
        }
    }

    fn gen_loop(&mut self, ctx: &mut BuildContext, cond: Option<&Expr>, body: &Stmt, f: Option<impl FnMut(&mut BuildContext)>) -> Result<()> {
        let loop_block = ctx.builder.create_block();
        let end_block = ctx.builder.create_block();
        if let Some(cond) = cond {
            let start_block = ctx.builder.create_block();
            ctx.builder.ins().jump(start_block, &[]);
            ctx.builder.switch_to_block(start_block);
            let cond = self.eval(ctx, cond)?.get(ctx).unwrap();
            let cond = self.bool_value(ctx, cond)?;
            let continue_block = if f.is_some() { ctx.builder.create_block() } else { start_block };
            ctx.builder.ins().brif(cond, loop_block, &[], end_block, &[]);
            ctx.builder.switch_to_block(loop_block);
            let body_terminated = self.gen_stmt(ctx, body, Some(end_block), Some(continue_block))?;
            if !body_terminated {
                ctx.builder.ins().jump(continue_block, &[]);
            }
            ctx.builder.seal_block(loop_block);
            f.map(|mut f| {
                ctx.builder.switch_to_block(continue_block);
                f(ctx);
                ctx.builder.ins().jump(start_block, &[]);
                ctx.builder.seal_block(continue_block);
            });
        } else {
            ctx.builder.ins().jump(loop_block, &[]);
            ctx.builder.switch_to_block(loop_block);
            let body_terminated = self.gen_stmt(ctx, body, Some(end_block), Some(loop_block))?;
            if !body_terminated {
                ctx.builder.ins().jump(loop_block, &[]);
            }
            ctx.builder.seal_block(loop_block);
        }
        ctx.builder.switch_to_block(end_block);
        Ok(())
    }

    pub(crate) fn gen_stmt(&mut self, ctx: &mut BuildContext, stmt: &Stmt, break_block: Option<Block>, continue_block: Option<Block>) -> Result<bool> {
        match &stmt.kind {
            StmtKind::Expr(expr, _) => {
                let _ = self.eval(ctx, expr)?;
            }
            StmtKind::Break => {
                ctx.builder.ins().jump(break_block.unwrap(), &[]);
                return Ok(true);
            }
            StmtKind::Continue => {
                ctx.builder.ins().jump(continue_block.unwrap(), &[]);
                return Ok(true);
            }
            StmtKind::Return(expr) => {
                if let Some(expr) = expr {
                    let value = self.eval(ctx, expr)?;
                    let value = value.get(ctx);
                    self.return_value(ctx, value)?;
                } else {
                    self.return_value(ctx, None)?;
                }
                return Ok(true);
            }
            StmtKind::If { cond, then_body, else_body } => {
                self.declare_assigned_vars(ctx, then_body)?;
                if let Some(else_body) = else_body {
                    self.declare_assigned_vars(ctx, else_body)?;
                }
                let then_block = ctx.builder.create_block();
                let cond = self.eval(ctx, cond)?.get(ctx).ok_or(anyhow!("未知的条件 {:?}", cond))?;
                let cond = self.bool_value(ctx, cond)?;
                let mut end_block = None;
                if let Some(else_body) = else_body {
                    let else_block = ctx.builder.create_block();
                    ctx.builder.ins().brif(cond, then_block, &[], else_block, &[]);
                    ctx.builder.switch_to_block(then_block);
                    if !self.gen_stmt(ctx, then_body, break_block, continue_block)? {
                        let block = ctx.builder.create_block();
                        ctx.builder.ins().jump(block, &[]);
                        end_block = Some(block);
                    }
                    ctx.builder.switch_to_block(else_block);
                    if !self.gen_stmt(ctx, else_body, break_block, continue_block)? {
                        if end_block.is_none() {
                            end_block = Some(ctx.builder.create_block());
                        }
                        ctx.builder.ins().jump(end_block.unwrap(), &[]);
                    }
                    ctx.builder.seal_block(else_block);
                } else {
                    let block = ctx.builder.create_block();
                    ctx.builder.ins().brif(cond, then_block, &[], block, &[]);
                    end_block = Some(block);
                    ctx.builder.switch_to_block(then_block);
                    if !self.gen_stmt(ctx, then_body, break_block, continue_block)? {
                        ctx.builder.ins().jump(end_block.unwrap(), &[]); //如果不是返回指令 增加跳转到 end_block
                    }
                }
                if let Some(block) = end_block {
                    ctx.builder.switch_to_block(block);
                }
                ctx.builder.seal_block(then_block);
                return Ok(end_block.is_none());
            }
            StmtKind::Block(stmts) => {
                for (idx, stmt) in stmts.iter().enumerate() {
                    let r = self.gen_stmt(ctx, stmt, break_block, continue_block)?;
                    if idx == stmts.len() - 1 {
                        return Ok(r);
                    }
                }
            }
            StmtKind::While { cond, body } => {
                self.declare_assigned_vars(ctx, body)?;
                let no_loop: Option<fn(&mut BuildContext)> = None;
                self.gen_loop(ctx, Some(cond), body, no_loop)?;
            }
            StmtKind::Loop(body) => {
                self.declare_assigned_vars(ctx, body)?;
                let no_loop: Option<fn(&mut BuildContext)> = None;
                self.gen_loop(ctx, None, body, no_loop)?;
            }
            StmtKind::For { pat, range, body } => {
                if let ExprKind::Range { start, stop, inclusive } = &range.kind {
                    if let PatternKind::Var { idx, .. } = &pat.kind {
                        let start = self.eval(ctx, start)?;
                        ctx.set_var(*idx, start)?;
                        self.declare_assigned_vars(ctx, body)?;
                        let op = if *inclusive { BinaryOp::Le } else { BinaryOp::Lt };
                        let cond = Self::expr(ExprKind::Binary { left: Box::new(Self::expr(ExprKind::Var(*idx))), op, right: Box::new(stop.as_ref().clone()) });
                        self.gen_loop(
                            ctx,
                            Some(&cond),
                            body,
                            Some(|ctx: &mut BuildContext| {
                                let v = ctx.get_var(*idx).unwrap().get(ctx).unwrap();
                                let step = if v.1 == Type::I64 {
                                    ctx.builder.ins().iconst(types::I64, 1)
                                } else if v.1 == Type::I32 {
                                    ctx.builder.ins().iconst(types::I32, 1)
                                } else {
                                    panic!("{:?} 不能作为增量", v.1)
                                };
                                let vt = (ctx.builder.ins().iadd(v.0, step), v.1).into();
                                let _ = ctx.set_var(*idx, vt);
                            }),
                        )?;
                    }
                } else if let PatternKind::Var { idx, .. } = &pat.kind {
                    let vt = self.eval(ctx, range)?.get(ctx).unwrap();
                    if vt.1.is_any() {
                        let iter = self.call(ctx, self.get_method(&vt.1, "iter")?, vec![vt.0])?;
                        let next = self.get_method(&vt.1, "next")?;
                        let next_id = next.get_id()?;
                        let start = self.call(ctx, next, vec![iter.0])?;
                        ctx.set_var(*idx, start.into())?;
                        let cond = Self::expr(ExprKind::Binary { left: Box::new(Self::expr(ExprKind::Var(*idx))), op: BinaryOp::Ne, right: Box::new(Self::expr(ExprKind::Value(Dynamic::Null))) });
                        self.gen_loop(
                            ctx,
                            Some(&cond),
                            body,
                            Some(|ctx: &mut BuildContext| {
                                let fn_ref = ctx.get_fn_ref(next_id).unwrap();
                                let call_inst = ctx.builder.ins().call(fn_ref, &[iter.0]);
                                let ret = ctx.builder.inst_results(call_inst)[0];
                                let _ = ctx.set_var(*idx, (ret, Type::Any).into());
                            }),
                        )?;
                    }
                } else if let PatternKind::Tuple(pats) = &pat.kind {
                    let vt = self.eval(ctx, range)?.get(ctx).unwrap();
                    if vt.1.is_any() && pats.len() == 2 {
                        //暂时只处理 kv
                        let iter = self.call(ctx, self.get_method(&vt.1, "iter")?, vec![vt.0])?;
                        let next = self.get_method(&vt.1, "next")?;
                        let next_id = next.get_id()?;
                        let get_idx = self.get_method(&vt.1, "get_idx")?.get_id()?;

                        let start = self.call(ctx, next, vec![iter.0])?;
                        let key_idx = ctx.builder.ins().iconst(types::I64, 0);
                        let key = self.call(ctx, self.get_method(&start.1, "get_idx")?, vec![start.0, key_idx])?;
                        let value_idx = ctx.builder.ins().iconst(types::I64, 1);
                        let value = self.call(ctx, self.get_method(&start.1, "get_idx")?, vec![start.0, value_idx])?;
                        ctx.set_var(pats[0].var().unwrap(), key.into())?;
                        ctx.set_var(pats[1].var().unwrap(), value.into())?;
                        let cond = Self::expr(ExprKind::Binary { left: Box::new(Self::expr(ExprKind::Var(pats[0].var().unwrap()))), op: BinaryOp::Ne, right: Box::new(Self::expr(ExprKind::Value(Dynamic::Null))) });
                        self.gen_loop(
                            ctx,
                            Some(&cond),
                            body,
                            Some(|ctx: &mut BuildContext| {
                                let fn_ref = ctx.get_fn_ref(next_id).unwrap();
                                let call_inst = ctx.builder.ins().call(fn_ref, &[iter.0]);
                                let ret = ctx.builder.inst_results(call_inst)[0];

                                let fn_ref = ctx.get_fn_ref(get_idx).unwrap();
                                let call_inst = ctx.builder.ins().call(fn_ref, &[ret, key_idx]);
                                let key_ret = ctx.builder.inst_results(call_inst)[0];
                                let call_inst = ctx.builder.ins().call(fn_ref, &[ret, value_idx]);
                                let value_ret = ctx.builder.inst_results(call_inst)[0];

                                let _ = ctx.set_var(pats[0].var().unwrap(), (key_ret, Type::Any).into());
                                let _ = ctx.set_var(pats[1].var().unwrap(), (value_ret, Type::Any).into());
                            }),
                        )?;
                    }
                }
            }
            _ => {
                panic!("未实现 {:?}", stmt)
            }
        }
        Ok(false)
    }
}
