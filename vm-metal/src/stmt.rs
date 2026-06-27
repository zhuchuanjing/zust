use anyhow::{Result, anyhow, bail};
use dynamic::Type;
use parser::{BinaryOp, Expr, ExprKind, Pattern, PatternKind, Stmt, StmtKind, UnaryOp};
use std::{collections::BTreeMap, rc::Rc};

use crate::{
    context::{MetalCompiler, Value},
    util::sanitize_ident,
};

impl MetalCompiler {
    pub(crate) fn gen_stmt(&mut self, stmt: &Stmt) -> Result<Option<Value>> {
        match &stmt.kind {
            StmtKind::Block(stmts) => {
                let mut last = None;
                for stmt in stmts {
                    last = self.gen_stmt(stmt)?;
                }
                Ok(last)
            }
            StmtKind::Expr(expr, close) => {
                let value = self.gen_expr(expr)?;
                if *close { Ok(None) } else { Ok(Some(value)) }
            }
            StmtKind::Let { pat, value } => {
                let value = if let StmtKind::Expr(expr, _) = &value.kind { self.gen_expr(expr)? } else { self.gen_stmt(value)?.ok_or_else(|| anyhow!("let value must produce a value for pattern {pat:?}"))? };
                self.bind_pattern(pat, value)?;
                Ok(None)
            }
            StmtKind::Return(expr) => {
                let value = expr.as_ref().map(|expr| self.gen_expr(expr)).transpose()?;
                if let Some(result_name) = self.inline_return.clone() {
                    if let Some(value) = value {
                        let value = self.convert_code(value, Type::Any)?;
                        self.line(format!("{result_name} = {};", value.code));
                    }
                } else if !self.inline_stack.is_empty() {
                    // Direct codegen inlines user functions; a return in that inline body
                    // marks the end of the source function, not the Metal kernel.
                } else if let Some(value) = value {
                    if let Some(ret_buffer) = self.ret_buffer.clone() {
                        self.line(format!("{ret_buffer}[0] = {};", value.code));
                    }
                    self.line("return;");
                } else {
                    self.line("return;");
                }
                Ok(None)
            }
            StmtKind::If { cond, then_body, else_body } => {
                for (idx, ty) in self.missing_branch_assignments(then_body, else_body.as_deref()) {
                    let name = self.var_name(idx);
                    self.line(format!("{} {name};", self.msl_type(&ty)));
                    self.set_var(idx, name, ty);
                }
                let cond_value = self.gen_expr(cond)?;
                let cond = self.bool_expr(cond_value)?;
                self.line(format!("if ({}) {{", cond.code));
                self.indent += 1;
                self.gen_stmt(then_body)?;
                self.indent -= 1;
                if let Some(else_body) = else_body {
                    self.line("} else {");
                    self.indent += 1;
                    self.gen_stmt(else_body)?;
                    self.indent -= 1;
                }
                self.line("}");
                Ok(None)
            }
            StmtKind::While { cond, body } => {
                let cond_value = self.gen_expr(cond)?;
                let cond = self.bool_expr(cond_value)?;
                self.line(format!("while ({}) {{", cond.code));
                self.indent += 1;
                self.gen_stmt(body)?;
                self.indent -= 1;
                self.line("}");
                Ok(None)
            }
            StmtKind::Loop(body) => {
                // 无条件 `loop { ... }` → MSL `while (true) { ... }`,退出靠 body
                // 内部的 `break` / `return`。
                self.line("while (true) {");
                self.indent += 1;
                self.gen_stmt(body)?;
                self.indent -= 1;
                self.line("}");
                Ok(None)
            }
            StmtKind::For { pat, range, body } => self.gen_for(pat, range, body),
            StmtKind::Break => {
                self.line("break;");
                Ok(None)
            }
            StmtKind::Continue => {
                self.line("continue;");
                Ok(None)
            }
            StmtKind::Static { .. } => Ok(None),
            StmtKind::Import { .. } => Ok(None),
            StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Impl { .. } | StmtKind::Const { .. } => bail!("statement is not supported by vm-metal yet: {stmt:?}"),
        }
    }

    pub(crate) fn gen_for(&mut self, pat: &Pattern, range: &Expr, body: &Stmt) -> Result<Option<Value>> {
        let ExprKind::Range { start, stop, inclusive } = &range.kind else {
            bail!("Metal for loop requires a range expression");
        };
        let start = self.gen_expr(start)?;
        let stop = self.gen_expr(stop)?;
        let idx_ty = if !stop.ty.is_any() { stop.ty.clone() } else { start.ty.clone() };
        let idx_name = match &pat.kind {
            PatternKind::Var { idx, .. } => {
                let name = self.var_name(*idx as usize);
                self.set_var(*idx as usize, name.clone(), idx_ty.clone());
                name
            }
            PatternKind::Ident { name, .. } => sanitize_ident(name),
            PatternKind::Wildcard => self.fresh("idx"),
            _ => bail!("unsupported Metal for loop pattern: {pat:?}"),
        };
        let op = if *inclusive { "<=" } else { "<" };
        self.line(format!("for ({} {idx_name} = {}; {idx_name} {op} {}; {idx_name} += {}) {{", self.msl_type(&idx_ty), start.code, stop.code, self.one_literal(&idx_ty)));
        self.indent += 1;
        self.gen_stmt(body)?;
        self.indent -= 1;
        self.line("}");
        Ok(None)
    }

    pub(crate) fn bind_pattern(&mut self, pat: &Pattern, value: Value) -> Result<()> {
        match &pat.kind {
            PatternKind::Var { idx, ty } => {
                let ty = if ty.is_any() { self.resolve_type(&value.ty) } else { self.resolve_type(ty) };
                let name = self.var_name(*idx as usize);
                let value = self.convert_code(value, ty.clone())?;
                self.line(format!("{} {name} = {};", self.msl_type(&ty), value.code));
                self.set_var(*idx as usize, name, ty);
                Ok(())
            }
            PatternKind::Ident { name, ty } => {
                let ty = if ty.is_any() { self.resolve_type(&value.ty) } else { self.resolve_type(ty) };
                let idx = self.vars.len();
                let name_str = sanitize_ident(name);
                let value = self.convert_code(value, ty.clone())?;
                self.line(format!("{} {name_str} = {};", self.msl_type(&ty), value.code));
                self.set_var(idx, name_str, ty);
                self.names[idx] = Some(name.clone());
                Ok(())
            }
            PatternKind::Wildcard => Ok(()),
            other => bail!("unsupported Metal let pattern: {other:?}"),
        }
    }

    pub(crate) fn missing_branch_assignments(&self, then_body: &Stmt, else_body: Option<&Stmt>) -> Vec<(usize, Type)> {
        let mut out = BTreeMap::new();
        self.collect_missing_assignments(then_body, &mut out);
        if let Some(else_body) = else_body {
            self.collect_missing_assignments(else_body, &mut out);
        }
        out.into_iter().collect()
    }

    pub(crate) fn collect_missing_assignments(&self, stmt: &Stmt, out: &mut BTreeMap<usize, Type>) {
        match &stmt.kind {
            StmtKind::Block(stmts) => {
                for stmt in stmts {
                    self.collect_missing_assignments(stmt, out);
                }
            }
            StmtKind::Expr(expr, _) => self.collect_missing_assignments_expr(expr, out),
            StmtKind::Let { value, .. } => self.collect_missing_assignments(value, out),
            StmtKind::If { then_body, else_body, .. } => {
                self.collect_missing_assignments(then_body, out);
                if let Some(else_body) = else_body {
                    self.collect_missing_assignments(else_body, out);
                }
            }
            StmtKind::While { body, .. } | StmtKind::Loop(body) => self.collect_missing_assignments(body, out),
            StmtKind::For { body, .. } => self.collect_missing_assignments(body, out),
            StmtKind::Return(_)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Fn { .. }
            | StmtKind::Struct { .. }
            | StmtKind::Impl { .. }
            | StmtKind::Static { .. }
            | StmtKind::Const { .. }
            | StmtKind::Import { .. } => {}
        }
    }

    pub(crate) fn collect_missing_assignments_expr(&self, expr: &Expr, out: &mut BTreeMap<usize, Type>) {
        match &expr.kind {
            ExprKind::Binary { left, op, right } if *op == BinaryOp::Assign || op.is_assign() => {
                if let ExprKind::Var(idx) = &left.kind {
                    let idx = *idx as usize;
                    if self.vars.get(idx).and_then(Clone::clone).is_none() {
                        out.entry(idx).or_insert_with(|| self.infer_expr_ty(right).unwrap_or(Type::U32));
                    }
                } else {
                    self.collect_missing_assignments_expr(left, out);
                }
                self.collect_missing_assignments_expr(right, out);
            }
            ExprKind::Binary { left, right, .. } => {
                self.collect_missing_assignments_expr(left, out);
                self.collect_missing_assignments_expr(right, out);
            }
            ExprKind::Unary { value, .. } | ExprKind::Typed { value, .. } | ExprKind::Repeat { value, .. } => self.collect_missing_assignments_expr(value, out),
            ExprKind::Generic { obj, .. } => self.collect_missing_assignments_expr(obj, out),
            ExprKind::TypedMethod { obj, .. } => self.collect_missing_assignments_expr(obj, out),
            ExprKind::Call { obj, params } => {
                self.collect_missing_assignments_expr(obj, out);
                for param in params {
                    self.collect_missing_assignments_expr(param, out);
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.collect_missing_assignments_expr(item, out);
                }
            }
            ExprKind::Dict(items) => {
                for (_, value) in items {
                    self.collect_missing_assignments_expr(value, out);
                }
            }
            ExprKind::Range { start, stop, .. } => {
                self.collect_missing_assignments_expr(start, out);
                self.collect_missing_assignments_expr(stop, out);
            }
            ExprKind::Stmt(stmt) | ExprKind::Closure { body: stmt, .. } => self.collect_missing_assignments(stmt, out),
            ExprKind::Id(_, receiver) => {
                if let Some(receiver) = receiver {
                    self.collect_missing_assignments_expr(receiver, out);
                }
            }
            ExprKind::Value(_) | ExprKind::Const(_) | ExprKind::Ident(_) | ExprKind::Var(_) | ExprKind::Capture(_) | ExprKind::Assoc { .. } | ExprKind::AssocId { .. } | ExprKind::Null => {}
        }
    }

    pub(crate) fn infer_expr_ty(&self, expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Value(value) => Some(value.get_type()),
            ExprKind::Typed { ty, .. } => Some(self.resolve_type(ty)),
            ExprKind::Var(idx) => self.vars.get(*idx as usize).and_then(Clone::clone).map(|var| var.ty),
            ExprKind::Unary { op: UnaryOp::Not, value } => self.infer_expr_ty(value).map(|ty| if ty.is_int() || ty.is_uint() { ty } else { Type::Bool }),
            ExprKind::Unary { value, .. } => self.infer_expr_ty(value),
            ExprKind::Binary { left, op, right } => {
                if op.is_logic() {
                    Some(Type::Bool)
                } else if *op == BinaryOp::Idx {
                    match self.infer_expr_ty(left)? {
                        Type::Vec(elem, _) | Type::Array(elem, _) => Some(elem.as_ref().clone()),
                        Type::Struct { fields, .. } => {
                            let idx = self.infer_const_u32(right)? as usize;
                            fields.get(idx).map(|(_, ty)| ty.clone())
                        }
                        _ => None,
                    }
                } else {
                    Some(self.infer_expr_ty(left)? + self.infer_expr_ty(right)?)
                }
            }
            ExprKind::Id(id, None) => self.workgroup_static_tys.get(id).map(|ty| self.resolve_type(ty)),
            ExprKind::Tuple(items) | ExprKind::List(items) if !items.is_empty() && items.len() <= 4 => Some(Type::Vec(Rc::new(self.infer_expr_ty(&items[0])?), items.len() as u32)),
            _ => None,
        }
    }

    pub(crate) fn infer_const_u32(&self, expr: &Expr) -> Option<u32> {
        match &expr.kind {
            ExprKind::Value(value) => value.as_uint().map(|value| value as u32).or_else(|| value.as_int().and_then(|value| (value >= 0).then_some(value as u32))),
            ExprKind::Const(idx) => self.compiler.consts.get_index(*idx).and_then(|(_, value)| value.as_uint().map(|value| value as u32).or_else(|| value.as_int().and_then(|value| (value >= 0).then_some(value as u32)))),
            _ => None,
        }
    }
}
