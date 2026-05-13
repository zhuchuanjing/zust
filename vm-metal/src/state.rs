use anyhow::{Result, anyhow};
use dynamic::Type;

use crate::context::{MetalCompiler, Value, Var};

impl MetalCompiler {
    pub(crate) fn get_var(&self, idx: usize) -> Result<Value> {
        self.vars.get(idx).and_then(Clone::clone).map(|var| Value { code: var.name, ty: var.ty }).ok_or_else(|| anyhow!("Metal variable {idx} not found"))
    }

    pub(crate) fn get_named_var(&self, name: &str) -> Result<Value> {
        let idx = self.names.iter().enumerate().rev().find_map(|(idx, existing)| if existing.as_deref() == Some(name) { Some(idx) } else { None }).ok_or_else(|| anyhow!("Metal identifier {name} not found"))?;
        self.get_var(idx)
    }

    pub(crate) fn set_var(&mut self, idx: usize, name: String, ty: Type) {
        if idx >= self.vars.len() {
            self.vars.resize(idx + 1, None);
        }
        if idx >= self.names.len() {
            self.names.resize(idx + 1, None);
        }
        self.vars[idx] = Some(Var { name, ty });
    }

    pub(crate) fn var_name(&self, idx: usize) -> String {
        format!("zv_{idx}")
    }

    pub(crate) fn fresh(&mut self, prefix: &str) -> String {
        let id = self.tmp;
        self.tmp += 1;
        format!("zust_{prefix}_{id}")
    }

    pub(crate) fn line(&mut self, line: impl AsRef<str>) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(line.as_ref());
        self.out.push('\n');
    }
}
