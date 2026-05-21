use anyhow::{Result, anyhow, bail};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Pattern, PatternKind, Stmt, StmtKind};
use rspirv::dr::Operand;
use std::collections::BTreeSet;

use crate::context::{Phi, SpirvCompiler, SpirvTy, Value};

impl SpirvCompiler {
    pub(crate) fn gen_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>> {
        match &stmt.kind {
            StmtKind::Block(stmts) => {
                let mut last = None;
                for stmt in stmts {
                    last = self.gen_stmt(stmt)?;
                    self.clear_statement_temps();
                    if self.current_block.is_none() {
                        break;
                    }
                }
                Ok(last)
            }
            StmtKind::Expr(expr, close) => {
                let value = self.gen_expr(expr)?;
                Ok(if *close { None } else { Some(value) })
            }
            StmtKind::Let { pat, value } => {
                let value =
                    if let StmtKind::Expr(expr, _) = &value.kind { self.gen_expr(expr)? } else { self.gen_stmt(value)?.ok_or_else(|| anyhow!("let value must produce a value for pattern {pat:?} from {value:?}"))? };
                self.bind_pattern(pat, value)?;
                Ok(None)
            }
            StmtKind::Return(expr) => {
                let value = expr.as_ref().map(|expr| self.gen_expr(expr)).transpose()?;
                Ok(value)
            }
            StmtKind::If { cond, then_body, else_body } => self.gen_if(cond, then_body, else_body.as_deref()),
            StmtKind::While { cond, body } => {
                self.gen_while(cond, body)?;
                Ok(None)
            }
            StmtKind::For { pat, range, body } => self.gen_for(pat, range, body),
            StmtKind::Break => {
                let (break_id, _) = self.loop_stack.last().copied().ok_or_else(|| anyhow!("break outside loop"))?;
                self.builder.branch(break_id)?;
                self.current_block = None;
                Ok(None)
            }
            StmtKind::Continue => {
                let (_, continue_id) = self.loop_stack.last().copied().ok_or_else(|| anyhow!("continue outside loop"))?;
                self.builder.branch(continue_id)?;
                self.current_block = None;
                Ok(None)
            }
            StmtKind::Static { .. } => Ok(None),
            StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Impl { .. } | StmtKind::Const { .. } | StmtKind::Loop(_) => bail!("statement is not supported by vm-spirv yet: {stmt:?}"),
        }
    }

    pub(crate) fn bind_pattern(&mut self, pat: &Pattern, value: Value) -> Result<()> {
        match &pat.kind {
            PatternKind::Var { idx, ty } => {
                let ty = self.resolve_type(ty);
                let value = if ty.is_any() { value } else { self.convert(value, ty)? };
                self.set_var_lazy(*idx as usize, value);
                Ok(())
            }
            PatternKind::Ident { .. } | PatternKind::Wildcard => {
                let idx = self.vars.len();
                self.set_var_lazy(idx, value);
                if let PatternKind::Ident { name, .. } = &pat.kind {
                    self.names[idx] = Some(name.clone());
                }
                Ok(())
            }
            other => bail!("unsupported SPIR-V let pattern: {other:?}"),
        }
    }

    pub(crate) fn gen_if(&mut self, cond: &Expr, then_body: &Stmt, else_body: Option<&Stmt>) -> Result<Option<Value>> {
        let cond_value = self.gen_expr(cond)?;
        let cond = self.bool_value(cond_value)?;
        let header = self.current_block.ok_or_else(|| anyhow!("if without active block"))?;
        let mut assigned_vars = Self::assigned_var_indices(then_body);
        if let Some(else_body) = else_body {
            assigned_vars.extend(Self::assigned_var_indices(else_body));
        }
        self.materialize_assigned_aggregate_vars(&assigned_vars)?;
        let then_label = self.builder.id();
        let else_label = else_body.map(|_| self.builder.id());
        let merge_label = self.builder.id();
        self.builder.selection_merge(merge_label, spirv::SelectionControl::NONE)?;
        self.builder.branch_conditional(cond.id, then_label, else_label.unwrap_or(merge_label), None)?;

        let before = self.vars.clone();
        self.current_block = Some(self.builder.begin_block(Some(then_label))?);
        let then_value = self.gen_stmt(then_body)?;
        let then_vars = self.vars.clone();
        let then_block = self.current_block;
        if then_block.is_some() {
            self.builder.branch(merge_label)?;
        }

        self.vars = before.clone();
        let (else_value, else_vars, else_block) = if let Some(else_body) = else_body {
            self.current_block = Some(self.builder.begin_block(Some(else_label.unwrap()))?);
            let value = self.gen_stmt(else_body)?;
            let vars = self.vars.clone();
            let block = self.current_block;
            if block.is_some() {
                self.builder.branch(merge_label)?;
            }
            (value, vars, block)
        } else {
            (None, before.clone(), Some(header))
        };

        self.current_block = Some(self.builder.begin_block(Some(merge_label))?);
        self.vars = self.merge_vars(before, then_vars, then_block, else_vars, else_block)?;

        match (then_value, else_value) {
            (Some(t), Some(e)) => {
                let e = self.convert(e, t.ty.clone())?;
                let ty_id = self.get_type(SpirvTy::Value(t.ty.clone()));
                let id = self.builder.phi(ty_id, None, [(t.id, then_block.unwrap()), (e.id, else_block.unwrap())])?;
                Ok(Some(Value { id, ty: t.ty }))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn gen_while(&mut self, cond: &Expr, body: &Stmt) -> Result<()> {
        let pre_header = self.current_block.ok_or_else(|| anyhow!("while without active block"))?;
        let header = self.builder.id();
        let body_label = self.builder.id();
        let continue_label = self.builder.id();
        let merge_label = self.builder.id();
        let assigned_vars = Self::assigned_var_indices(body);
        self.materialize_assigned_aggregate_vars(&assigned_vars)?;
        let before = self.vars.clone();
        self.builder.branch(header)?;

        self.current_block = Some(self.builder.begin_block(Some(header))?);
        let mut phis = self.loop_phi_placeholders(&before, pre_header);
        phis.retain(|phi| assigned_vars.contains(&phi.idx));
        for phi in &phis {
            let ty_id = self.get_type(SpirvTy::Value(phi.ty.clone()));
            let id = self.builder.phi(ty_id, Some(phi.result_id), phi.incoming.clone())?;
            self.set_var(phi.idx, Value { id, ty: phi.ty.clone() });
        }
        let cond_value = self.gen_expr(cond)?;
        let cond = self.bool_value(cond_value)?;
        self.builder.loop_merge(merge_label, continue_label, spirv::LoopControl::NONE, [])?;
        self.builder.branch_conditional(cond.id, body_label, merge_label, None)?;

        self.current_block = Some(self.builder.begin_block(Some(body_label))?);
        self.loop_stack.push((merge_label, continue_label));
        self.gen_stmt(body)?;
        self.loop_stack.pop();
        if self.current_block.is_some() {
            self.builder.branch(continue_label)?;
        }

        self.current_block = Some(self.builder.begin_block(Some(continue_label))?);
        for phi in &mut phis {
            if let Some(value) = self.vars.get(phi.idx).and_then(Clone::clone) {
                let value = self.convert(value, phi.ty.clone())?;
                phi.incoming.push((value.id, continue_label));
                self.set_var(phi.idx, Value { id: phi.result_id, ty: phi.ty.clone() });
            }
        }
        self.patch_phi_incoming(&phis);
        self.builder.branch(header)?;

        self.current_block = Some(self.builder.begin_block(Some(merge_label))?);
        Ok(())
    }

    pub(crate) fn gen_for(&mut self, pat: &Pattern, range: &Expr, body: &Stmt) -> Result<Option<Value>> {
        // Extract range start and stop
        let (start_expr, stop_expr, inclusive) = match &range.kind {
            ExprKind::Range { start, stop, inclusive } => (start.as_ref(), stop.as_ref(), *inclusive),
            _ => bail!("SPIR-V for loop requires a range expression"),
        };

        // Get types from pattern if available
        let pat_ty = match &pat.kind {
            PatternKind::Var { idx: _, ty } => ty.clone(),
            PatternKind::Ident { name: _, ty } => ty.clone(),
            PatternKind::Wildcard => Type::Any,
            _ => Type::Any,
        };

        // Generate start and stop values
        let mut start_val = self.gen_expr(start_expr)?;
        let mut stop_val = self.gen_expr(stop_expr)?;

        // Infer types - prioritize concrete types from expressions over pattern type
        // Pattern type may be Any during compilation, so rely more on actual values
        let idx_ty = if !start_val.ty.is_any() && !stop_val.ty.is_any() {
            // Both expressions have concrete types - use stop's type (usually the bound)
            start_val = self.convert(start_val, stop_val.ty.clone())?;
            stop_val.ty.clone()
        } else if !pat_ty.is_any() {
            pat_ty
        } else if start_val.ty.is_any() && !stop_val.ty.is_any() {
            start_val = self.convert(start_val, stop_val.ty.clone())?;
            stop_val.ty.clone()
        } else if !start_val.ty.is_any() && stop_val.ty.is_any() {
            stop_val = self.convert(stop_val, start_val.ty.clone())?;
            start_val.ty.clone()
        } else if start_val.ty.is_any() && stop_val.ty.is_any() {
            // Default to u32 if both are Any
            start_val = self.convert(start_val, Type::U32)?;
            stop_val = self.convert(stop_val, Type::U32)?;
            Type::U32
        } else {
            start_val.ty.clone()
        };

        // Add loop variable with initial value
        // Use the inferred idx_ty, not the pattern's type annotation
        let idx_var = match &pat.kind {
            PatternKind::Var { idx, ty: _ } => {
                let val = self.convert(start_val, idx_ty.clone())?;
                self.set_var(*idx as usize, val);
                *idx as usize
            }
            PatternKind::Ident { name, ty: _ } => {
                let val = self.convert(start_val, idx_ty.clone())?;
                let idx = self.vars.len();
                self.set_var(idx, val);
                self.names[idx] = Some(name.clone());
                idx
            }
            PatternKind::Wildcard => {
                let val = self.convert(start_val, idx_ty.clone())?;
                let idx = self.vars.len();
                self.set_var(idx, val);
                idx
            }
            _ => bail!("unsupported for loop pattern: {:?}", pat),
        };

        // Build the while loop structure
        let pre_header = self.current_block.ok_or_else(|| anyhow!("for without active block"))?;
        let header = self.builder.id();
        let body_label = self.builder.id();
        let continue_label = self.builder.id();
        let merge_label = self.builder.id();
        let assigned_vars = Self::assigned_var_indices(body);
        self.materialize_assigned_aggregate_vars(&assigned_vars)?;
        let before = self.vars.clone();
        self.builder.branch(header)?;

        // Header block: phi for loop variable and condition check
        self.current_block = Some(self.builder.begin_block(Some(header))?);
        let mut phis = self.loop_phi_placeholders(&before, pre_header);
        phis.retain(|phi| phi.idx != idx_var);
        phis.retain(|phi| assigned_vars.contains(&phi.idx));
        // Add phi for the loop index variable
        let mut idx_phi = if let Some(current) = self.vars.get(idx_var).and_then(Clone::clone) {
            let result_id = self.builder.id();
            let ty_id = self.get_type(SpirvTy::Value(idx_ty.clone()));
            let id = self.builder.phi(ty_id, Some(result_id), vec![(current.id, pre_header)])?;
            let val = Value { id, ty: idx_ty.clone() };
            self.set_var(idx_var, val.clone());
            Some(Phi { idx: idx_var, ty: idx_ty.clone(), result_id, incoming: vec![(current.id, pre_header)] })
        } else {
            None
        };

        for phi in &phis {
            let ty_id = self.get_type(SpirvTy::Value(phi.ty.clone()));
            let id = self.builder.phi(ty_id, Some(phi.result_id), phi.incoming.clone())?;
            self.set_var(phi.idx, Value { id, ty: phi.ty.clone() });
        }

        // Generate condition: idx < stop (or idx <= stop for inclusive)
        let idx_val = self.vars.get(idx_var).and_then(Clone::clone).ok_or_else(|| anyhow!("loop index not found"))?;
        let cond = if inclusive { self.binary(idx_val.clone(), &BinaryOp::Le, stop_val.clone())? } else { self.binary(idx_val.clone(), &BinaryOp::Lt, stop_val.clone())? };
        let cond = self.bool_value(cond)?;
        self.builder.loop_merge(merge_label, continue_label, spirv::LoopControl::NONE, [])?;
        self.builder.branch_conditional(cond.id, body_label, merge_label, None)?;

        // Body block
        self.current_block = Some(self.builder.begin_block(Some(body_label))?);
        self.loop_stack.push((merge_label, continue_label));
        self.gen_stmt(body)?;
        self.loop_stack.pop();
        if self.current_block.is_some() {
            self.builder.branch(continue_label)?;
        }

        // Continue block: increment idx and update phis
        self.current_block = Some(self.builder.begin_block(Some(continue_label))?);

        // Increment idx
        let one = self.const_dynamic(if idx_ty.is_uint() {
            Dynamic::U32(1)
        } else if idx_ty.is_int() {
            Dynamic::I32(1)
        } else {
            Dynamic::I32(1)
        })?;
        let one = self.convert(one, idx_ty.clone())?;
        let current_idx = self.vars.get(idx_var).and_then(Clone::clone).ok_or_else(|| anyhow!("loop index not found in continue"))?;
        let new_idx = self.binary(current_idx, &BinaryOp::Add, one)?;
        self.set_var(idx_var, new_idx.clone());

        // Update phis
        for phi in &mut phis {
            if let Some(value) = self.vars.get(phi.idx).and_then(Clone::clone) {
                let value = self.convert(value, phi.ty.clone())?;
                phi.incoming.push((value.id, continue_label));
                self.set_var(phi.idx, Value { id: phi.result_id, ty: phi.ty.clone() });
            }
        }
        // Update idx phi
        if let Some(ref mut phi) = idx_phi {
            phi.incoming.push((new_idx.id, continue_label));
            self.set_var(idx_var, Value { id: phi.result_id, ty: phi.ty.clone() });
        }

        // Patch phi instructions
        if let Some(ref phi) = idx_phi {
            self.patch_single_phi(phi);
        }
        self.patch_phi_incoming(&phis);
        self.builder.branch(header)?;

        // Merge block
        self.current_block = Some(self.builder.begin_block(Some(merge_label))?);
        Ok(None)
    }

    pub(crate) fn patch_single_phi(&mut self, phi: &Phi) {
        for inst in self.builder.module_mut().all_inst_iter_mut() {
            if inst.result_id == Some(phi.result_id) {
                inst.operands.clear();
                for (id, label) in &phi.incoming {
                    inst.operands.push(Operand::IdRef(*id));
                    inst.operands.push(Operand::IdRef(*label));
                }
            }
        }
    }

    pub(crate) fn assigned_var_indices(stmt: &Stmt) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        Self::collect_assigned_vars_stmt(stmt, &mut out);
        out
    }

    pub(crate) fn collect_assigned_vars_stmt(stmt: &Stmt, out: &mut BTreeSet<usize>) {
        match &stmt.kind {
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    Self::collect_assigned_vars_stmt(stmt, out);
                }
            }
            StmtKind::Expr(expr, _) => Self::collect_assigned_vars_expr(expr, out),
            StmtKind::Let { value, .. } => Self::collect_assigned_vars_stmt(value, out),
            StmtKind::Return(expr) => {
                if let Some(expr) = expr {
                    Self::collect_assigned_vars_expr(expr, out);
                }
            }
            StmtKind::If { cond, then_body, else_body } => {
                Self::collect_assigned_vars_expr(cond, out);
                Self::collect_assigned_vars_stmt(then_body, out);
                if let Some(else_body) = else_body {
                    Self::collect_assigned_vars_stmt(else_body, out);
                }
            }
            StmtKind::While { cond, body } => {
                Self::collect_assigned_vars_expr(cond, out);
                Self::collect_assigned_vars_stmt(body, out);
            }
            StmtKind::For { range, body, .. } => {
                Self::collect_assigned_vars_expr(range, out);
                Self::collect_assigned_vars_stmt(body, out);
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Impl { .. } | StmtKind::Static { .. } | StmtKind::Const { .. } | StmtKind::Loop(_) => {}
        }
    }

    pub(crate) fn collect_assigned_vars_expr(expr: &Expr, out: &mut BTreeSet<usize>) {
        match &expr.kind {
            ExprKind::Unary { value, .. } | ExprKind::Typed { value, .. } | ExprKind::Repeat { value, .. } => Self::collect_assigned_vars_expr(value, out),
            ExprKind::TypedMethod { obj, .. } => Self::collect_assigned_vars_expr(obj, out),
            ExprKind::Binary { left, op, right } => {
                if *op == BinaryOp::Assign || op.is_assign() {
                    Self::collect_assignment_target_vars(left, out);
                } else {
                    Self::collect_assigned_vars_expr(left, out);
                }
                Self::collect_assigned_vars_expr(right, out);
            }
            ExprKind::Call { obj, params } => {
                Self::collect_assigned_vars_expr(obj, out);
                for param in params {
                    Self::collect_assigned_vars_expr(param, out);
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    Self::collect_assigned_vars_expr(item, out);
                }
            }
            ExprKind::Dict(items) => {
                for (_, value) in items {
                    Self::collect_assigned_vars_expr(value, out);
                }
            }
            ExprKind::Id(_, receiver) => {
                if let Some(receiver) = receiver {
                    Self::collect_assigned_vars_expr(receiver, out);
                }
            }
            ExprKind::Range { start, stop, .. } => {
                Self::collect_assigned_vars_expr(start, out);
                Self::collect_assigned_vars_expr(stop, out);
            }
            ExprKind::Stmt(stmt) => Self::collect_assigned_vars_stmt(stmt, out),
            ExprKind::Closure { body, .. } => Self::collect_assigned_vars_stmt(body, out),
            ExprKind::Value(_) | ExprKind::Const(_) | ExprKind::Ident(_) | ExprKind::Var(_) | ExprKind::AssocId { .. } | ExprKind::Null | ExprKind::Capture(_) | ExprKind::Assoc { .. } => {}
        }
    }

    pub(crate) fn collect_assignment_target_vars(expr: &Expr, out: &mut BTreeSet<usize>) {
        match &expr.kind {
            ExprKind::Var(idx) => {
                out.insert(*idx as usize);
            }
            ExprKind::Binary { left, op: BinaryOp::Idx, .. } => Self::collect_assignment_target_vars(left, out),
            ExprKind::Typed { value, .. } => Self::collect_assignment_target_vars(value, out),
            _ => Self::collect_assigned_vars_expr(expr, out),
        }
    }

    pub(crate) fn merge_vars(&mut self, before: Vec<Option<Value>>, then_vars: Vec<Option<Value>>, then_block: Option<u32>, else_vars: Vec<Option<Value>>, else_block: Option<u32>) -> Result<Vec<Option<Value>>> {
        let len = before.len().max(then_vars.len()).max(else_vars.len());
        let mut merged = Vec::with_capacity(len);
        for idx in 0..len {
            let t = then_vars.get(idx).cloned().flatten();
            let e = else_vars.get(idx).cloned().flatten();
            let b = before.get(idx).cloned().flatten();
            match (t, e, then_block, else_block) {
                (Some(t), Some(e), Some(tb), Some(eb)) if t.id != e.id => {
                    let e = self.convert(e, t.ty.clone())?;
                    let ty_id = self.get_type(SpirvTy::Value(t.ty.clone()));
                    let id = self.builder.phi(ty_id, None, [(t.id, tb), (e.id, eb)])?;
                    merged.push(Some(Value { id, ty: t.ty }));
                }
                (Some(t), _, _, _) => merged.push(Some(t)),
                (_, Some(e), _, _) => merged.push(Some(e)),
                _ => merged.push(b),
            }
        }
        Ok(merged)
    }

    pub(crate) fn loop_phi_placeholders(&mut self, vars: &[Option<Value>], label: u32) -> Vec<Phi> {
        vars.iter()
            .enumerate()
            .filter_map(|(idx, value)| {
                value.as_ref().filter(|v| !v.ty.is_void() && !Self::is_runtime_array(&v.ty) && !v.ty.is_array() && !v.ty.is_struct()).map(|value| Phi {
                    idx,
                    ty: value.ty.clone(),
                    result_id: self.builder.id(),
                    incoming: vec![(value.id, label)],
                })
            })
            .collect()
    }

    pub(crate) fn patch_phi_incoming(&mut self, phis: &[Phi]) {
        for inst in self.builder.module_mut().all_inst_iter_mut() {
            if let Some(phi) = phis.iter().find(|phi| inst.result_id == Some(phi.result_id)) {
                inst.operands.clear();
                for (id, label) in &phi.incoming {
                    inst.operands.push(Operand::IdRef(*id));
                    inst.operands.push(Operand::IdRef(*label));
                }
            }
        }
    }
}
