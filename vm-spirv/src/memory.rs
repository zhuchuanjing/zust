use anyhow::{Result, anyhow};
use dynamic::Type;
use parser::{BinaryOp, Expr, ExprKind};
use rspirv::dr::{InsertPoint, Instruction, Operand};
use spirv::{BuiltIn, Decoration, StorageClass};
use std::rc::Rc;

use crate::{
    api::BuiltinFn,
    context::{SpirvCompiler, SpirvTy, Value},
};

impl SpirvCompiler {
    pub(crate) fn add_storage_buffer(&mut self, ty: Type, binding: u32) -> Result<u32> {
        let ty = self.resolve_type(&ty);
        let struct_id = self.get_type(SpirvTy::Buffer(ty.clone()));
        let ptr_ty = self.builder.type_pointer(None, StorageClass::StorageBuffer, struct_id);
        let var = self.builder.variable(ptr_ty, None, StorageClass::StorageBuffer, None);
        self.builder.decorate(var, Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
        self.builder.decorate(var, Decoration::Binding, [Operand::LiteralBit32(binding)]);
        self.buffers.push(var);
        Ok(var)
    }

    pub(crate) fn add_workgroup_statics(&mut self) -> Result<()> {
        self.workgroup_statics.clear();
        let statics = self.workgroup_static_tys.clone();
        for (id, ty) in statics {
            let ty = self.resolve_type(&ty);
            let ptr_ty = self.get_type(SpirvTy::Pointer(ty.clone(), StorageClass::Workgroup));
            let var = self.builder.variable(ptr_ty, None, StorageClass::Workgroup, None);
            self.interfaces.push(var);
            self.workgroup_statics.insert(id, Value { id: var, ty });
        }
        Ok(())
    }

    pub(crate) fn load_buffer_value(&mut self, buffer: u32, ty: Type) -> Result<Value> {
        let ty = self.resolve_type(&ty);
        let zero = self.const_u32(0);
        let ptr_ty = self.get_type(SpirvTy::Pointer(ty.clone(), StorageClass::StorageBuffer));
        let ptr = self.builder.access_chain(ptr_ty, None, buffer, [zero])?;
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = self.builder.load(ty_id, None, ptr, None, None)?;
        Ok(Value { id, ty })
    }

    pub(crate) fn store_buffer_value(&mut self, buffer: u32, value: Value) -> Result<()> {
        let zero = self.const_u32(0);
        let ptr_ty = self.get_type(SpirvTy::Pointer(value.ty.clone(), StorageClass::StorageBuffer));
        let ptr = self.builder.access_chain(ptr_ty, None, buffer, [zero])?;
        self.builder.store(ptr, value.id, None, None)?;
        Ok(())
    }

    pub(crate) fn workgroup_static_ptr(&self, id: u32) -> Result<Value> {
        self.workgroup_statics.get(&id).cloned().ok_or_else(|| anyhow!("SPIR-V workgroup static {id} not found"))
    }

    pub(crate) fn load_workgroup_static(&mut self, id: u32) -> Result<Value> {
        let ptr = self.workgroup_static_ptr(id)?;
        let ty_id = self.get_type(SpirvTy::Value(ptr.ty.clone()));
        let id = self.builder.load(ty_id, None, ptr.id, None, None)?;
        Ok(Value { id, ty: ptr.ty })
    }

    pub(crate) fn get_builtin_input(&mut self, builtin: BuiltinFn) -> (u32, Type, u32) {
        if let Some(value) = self.builtin_vars.get(&builtin) {
            return value.clone();
        }
        let ty = Type::Vec(Rc::new(Type::U32), 3);
        let ptr_ty = self.get_type(SpirvTy::Pointer(ty.clone(), StorageClass::Input));
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let var = self.builder.variable(ptr_ty, None, StorageClass::Input, None);
        let built_in = match builtin {
            BuiltinFn::GroupId => BuiltIn::WorkgroupId,
            BuiltinFn::LocalId => BuiltIn::LocalInvocationId,
            BuiltinFn::Barrier | BuiltinFn::AtomicAdd => unreachable!("builtin has no input variable"),
        };
        self.builder.decorate(var, Decoration::BuiltIn, [Operand::BuiltIn(built_in)]);
        self.interfaces.push(var);
        self.builtin_vars.insert(builtin, (var, ty.clone(), ty_id));
        (var, ty, ty_id)
    }

    pub(crate) fn set_var(&mut self, idx: usize, value: Value) {
        self.ensure_var_slot(idx);
        if value.ty.is_array() || value.ty.is_struct() {
            if let Some(ptr) = self.local_ptrs[idx].clone() {
                self.builder.store(ptr.id, value.id, None, None).expect("store aggregate local");
            } else {
                let ptr = self.create_function_var(value.ty.clone()).expect("aggregate local variable");
                self.builder.store(ptr.id, value.id, None, None).expect("initialize aggregate local");
                self.local_ptrs[idx] = Some(ptr);
            }
            self.vars[idx] = None;
        } else {
            self.local_ptrs[idx] = None;
            self.vars[idx] = Some(value);
        }
    }

    pub(crate) fn set_var_lazy(&mut self, idx: usize, value: Value) {
        self.ensure_var_slot(idx);
        if value.ty.is_array() || value.ty.is_struct() {
            if let Some(ptr) = self.local_ptrs[idx].clone() {
                self.builder.store(ptr.id, value.id, None, None).expect("store aggregate local");
                self.vars[idx] = None;
            } else {
                self.vars[idx] = Some(value);
            }
        } else {
            self.local_ptrs[idx] = None;
            self.vars[idx] = Some(value);
        }
    }

    pub(crate) fn ensure_var_slot(&mut self, idx: usize) {
        if idx >= self.vars.len() {
            self.vars.resize(idx + 1, None);
        }
        if idx >= self.local_ptrs.len() {
            self.local_ptrs.resize(idx + 1, None);
        }
        if idx >= self.names.len() {
            self.names.resize(idx + 1, None);
        }
    }

    pub(crate) fn create_function_var(&mut self, ty: Type) -> Result<Value> {
        let ty = self.resolve_type(&ty);
        if let Some((_, values)) = self.function_var_pool.iter_mut().find(|(existing, values)| existing == &ty && !values.is_empty())
            && let Some(value) = values.pop()
        {
            return Ok(value);
        }
        let ptr_ty = self.get_type(SpirvTy::Pointer(ty.clone(), StorageClass::Function));
        let id = self.function_variable(ptr_ty)?;
        Ok(Value { id, ty })
    }

    pub(crate) fn materialize_var_ptr(&mut self, idx: usize) -> Result<Option<Value>> {
        if let Some(ptr) = self.local_ptrs.get(idx).and_then(Clone::clone) {
            return Ok(Some(ptr));
        }
        let Some(value) = self.vars.get(idx).and_then(Clone::clone) else {
            return Ok(None);
        };
        if !(value.ty.is_array() || value.ty.is_struct()) {
            return Ok(None);
        }
        self.ensure_var_slot(idx);
        let ptr = self.create_function_var(value.ty.clone())?;
        self.builder.store(ptr.id, value.id, None, None)?;
        self.local_ptrs[idx] = Some(ptr.clone());
        self.vars[idx] = None;
        Ok(Some(ptr))
    }

    pub(crate) fn materialize_temp_ptr(&mut self, value: Value) -> Result<Value> {
        if !(value.ty.is_array() || value.ty.is_struct()) {
            return Err(anyhow!("SPIR-V temporary pointer requires aggregate value, got {:?}", value.ty));
        }
        let ptr = self.create_function_var(value.ty.clone())?;
        self.builder.store(ptr.id, value.id, None, None)?;
        self.statement_temp_ptrs.push(ptr.clone());
        Ok(ptr)
    }

    pub(crate) fn materialize_assigned_aggregate_vars(&mut self, assigned: &std::collections::BTreeSet<usize>) -> Result<()> {
        for &idx in assigned {
            let Some(value) = self.vars.get(idx).and_then(Clone::clone) else {
                continue;
            };
            if value.ty.is_array() || value.ty.is_struct() {
                self.materialize_var_ptr(idx)?;
            }
        }
        Ok(())
    }

    pub(crate) fn function_variable(&mut self, ptr_ty: u32) -> Result<u32> {
        let selected_block = self.builder.selected_block();
        let id = self.builder.id();
        let inst = Instruction::new(spirv::Op::Variable, Some(ptr_ty), Some(id), vec![Operand::StorageClass(StorageClass::Function)]);
        self.builder.select_block(Some(0))?;
        self.builder.insert_into_block(InsertPoint::Begin, inst)?;
        self.builder.select_block(selected_block)?;
        Ok(id)
    }

    pub(crate) fn get_var(&mut self, idx: usize) -> Option<Value> {
        if let Some(ptr) = self.local_ptrs.get(idx).and_then(Clone::clone) {
            let ty_id = self.get_type(SpirvTy::Value(ptr.ty.clone()));
            let id = self.builder.load(ty_id, None, ptr.id, None, None).ok()?;
            return Some(Value { id, ty: ptr.ty });
        }
        self.vars.get(idx).and_then(Clone::clone)
    }

    pub(crate) fn expr_ptr(&mut self, expr: &Expr) -> Result<Option<Value>> {
        self.expr_ptr_with_materialize(expr, true)
    }

    pub(crate) fn expr_existing_ptr(&mut self, expr: &Expr) -> Result<Option<Value>> {
        self.expr_ptr_with_materialize(expr, false)
    }

    pub(crate) fn expr_ptr_with_materialize(&mut self, expr: &Expr, materialize: bool) -> Result<Option<Value>> {
        match &expr.kind {
            ExprKind::Var(idx) => {
                if materialize {
                    self.materialize_var_ptr(*idx as usize)
                } else {
                    Ok(self.local_ptrs.get(*idx as usize).and_then(Clone::clone))
                }
            }
            ExprKind::Ident(name) => Ok(self.names.iter().enumerate().rev().find_map(|(idx, existing)| if existing.as_deref() == Some(name) { self.local_ptrs.get(idx).and_then(Clone::clone) } else { None })),
            ExprKind::Binary { left, op: BinaryOp::Idx, right } => {
                if let Some(ptr) = self.expr_ptr_with_materialize(left, materialize)? {
                    let idx = self.gen_expr(right)?;
                    return self.index_local_ptr(ptr, idx).map(Some);
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn get_named_var(&mut self, name: &str) -> Result<Value> {
        let idx = self.names.iter().enumerate().rev().find_map(|(idx, existing)| if existing.as_deref() == Some(name) { Some(idx) } else { None }).ok_or_else(|| anyhow!("SPIR-V identifier {name} not found"))?;
        self.get_var(idx).ok_or_else(|| anyhow!("SPIR-V identifier {name} has no value"))
    }
}
