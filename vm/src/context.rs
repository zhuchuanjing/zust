use crate::get_type;
use anyhow::{Result, anyhow};
use cranelift::codegen::ir::FuncRef;
use cranelift::prelude::{FunctionBuilder, InstBuilder, Value, Variable, types};
use cranelift_module::FuncId;
use dynamic::{Dynamic, Type};

#[derive(Clone, Debug)]
pub enum LocalVar {
    None,
    Variable { var: Variable, ty: Type },
    Value { val: Value, ty: Type },
    Closure { id: u32, captures: Vec<(Value, Type)> },
}

impl Into<LocalVar> for (Value, Type) {
    fn into(self) -> LocalVar {
        LocalVar::Value { val: self.0, ty: self.1 }
    }
}

impl LocalVar {
    fn normalize_for_var(ctx: &mut BuildContext, val: Value, ty: &Type) -> Value {
        let Ok(expected) = get_type(ty) else {
            return val;
        };
        let actual = ctx.builder.func.dfg.value_type(val);
        if actual == expected {
            return val;
        }

        if ty.is_bool() && actual.bits() == 1 {
            let zero = ctx.builder.ins().iconst(types::I8, 0);
            let one = ctx.builder.ins().iconst(types::I8, 1);
            return ctx.builder.ins().select(val, one, zero);
        }

        if expected.is_int() && actual.is_int() {
            if actual.bits() > expected.bits() {
                return ctx.builder.ins().ireduce(expected, val);
            }
            if actual.bits() < expected.bits() {
                return if ty.is_uint() { ctx.builder.ins().uextend(expected, val) } else { ctx.builder.ins().sextend(expected, val) };
            }
        }

        if expected.is_float() && actual.is_float() {
            if actual.bits() > expected.bits() {
                return ctx.builder.ins().fdemote(expected, val);
            }
            if actual.bits() < expected.bits() {
                return ctx.builder.ins().fpromote(expected, val);
            }
        }

        val
    }

    pub fn is_closure(&self) -> bool {
        if let Self::Closure { .. } = self { true } else { false }
    }

    pub fn get_ty(&self) -> Type {
        match self {
            Self::Value { val: _, ty } => ty.clone(),
            Self::Variable { var: _, ty } => ty.clone(),
            _ => Type::Any,
        }
    }

    pub fn new(ctx: &mut BuildContext, val: Value, ty: Type) -> Result<Self> {
        let val = Self::normalize_for_var(ctx, val, &ty);
        let var = ctx.builder.declare_var(get_type(&ty)?);
        ctx.builder.def_var(var, val);
        Ok(Self::Variable { var, ty })
    }

    pub fn get(self, ctx: &mut BuildContext) -> Option<(Value, Type)> {
        match self {
            Self::Value { val, ty } => Some((val, ty)),
            Self::Variable { var, ty } => Some((ctx.builder.use_var(var), ty)),
            _ => None,
        }
    }

    pub fn set(&self, ctx: &mut BuildContext, val: Value) {
        if let Self::Variable { var, ty } = self {
            let val = Self::normalize_for_var(ctx, val, ty);
            ctx.builder.def_var(var.clone(), val);
        }
    }
}

//用来生成 函数代码的上下文
pub struct BuildContext<'a> {
    pub builder: FunctionBuilder<'a>,
    pub(crate) vars: Vec<LocalVar>,
    pub(crate) fn_refs: Vec<(FuncId, FuncRef)>,
    pub(crate) ret_ty: Type,
}

impl<'a> BuildContext<'a> {
    pub fn new(mut builder: FunctionBuilder<'a>, arg_tys: &[Type], ret_ty: Type) -> Result<Self> {
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        let mut vars = Vec::new();
        for (idx, ty) in arg_tys.iter().enumerate() {
            vars.push(LocalVar::Value { val: builder.block_params(entry_block)[idx], ty: ty.clone() });
        }
        Ok(Self { builder, vars, fn_refs: Vec::new(), ret_ty })
    }

    pub fn get_fn_ref(&mut self, fn_id: FuncId) -> Option<FuncRef> {
        self.fn_refs.iter().find_map(|f| if f.0 == fn_id { Some(f.1.clone()) } else { None })
    }

    pub fn get_var(&mut self, idx: u32) -> Result<LocalVar> {
        self.vars.get(idx as usize).cloned().ok_or(anyhow!("未发现变量 {}", idx))
    }

    pub fn get_var_ty(&self, idx: u32) -> Option<Type> {
        self.vars.get(idx as usize).map(|v| v.get_ty())
    }

    pub fn set_var(&mut self, idx: u32, val: LocalVar) -> Result<()> {
        if idx as usize == self.vars.len() {
            if val.is_closure() {
                self.vars.push(val);
            } else if let Some(vt) = val.get(self) {
                let v = LocalVar::new(self, vt.0, vt.1)?;
                self.vars.push(v);
            }
        } else if (idx as usize) < self.vars.len() {
            if val.is_closure() {
                self.vars[idx as usize] = val;
            } else if let Some(vt) = val.get(self) {
                if matches!(self.vars[idx as usize], LocalVar::None | LocalVar::Value { .. }) {
                    let v = LocalVar::new(self, vt.0, vt.1)?;
                    self.vars[idx as usize] = v;
                } else {
                    let v = self.vars[idx as usize].clone();
                    v.set(self, vt.0);
                }
            }
        } else {
            self.vars.resize(idx as usize, LocalVar::None);
            if val.is_closure() {
                self.vars.push(val);
            } else if let Some(vt) = val.get(self) {
                let v = LocalVar::new(self, vt.0, vt.1)?;
                self.vars.push(v);
            }
        }
        Ok(())
    }

    pub fn get_const(&mut self, v: &Dynamic) -> Result<(Value, Type)> {
        let ty = v.get_type();
        if ty.is_f32() {
            return Ok((self.builder.ins().f32const(v.as_float().unwrap() as f32), ty));
        } else if ty.is_f64() {
            return Ok((self.builder.ins().f64const(v.as_float().unwrap()), ty));
        } else if ty.is_int() || ty.is_uint() {
            return match ty.width() {
                1 => Ok((self.builder.ins().iconst(types::I8, v.as_int().unwrap()), ty)),
                2 => Ok((self.builder.ins().iconst(types::I16, v.as_int().unwrap()), ty)),
                4 => Ok((self.builder.ins().iconst(types::I32, v.as_int().unwrap()), ty)),
                8 => Ok((self.builder.ins().iconst(types::I64, v.as_int().unwrap()), ty)),
                _ => panic!("const {:?}", v),
            };
        } else if ty.is_bool() {
            return if v.is_true() { Ok((self.builder.ins().iconst(types::I8, 1), ty)) } else { Ok((self.builder.ins().iconst(types::I8, 0), ty)) };
        }
        Err(anyhow!("未实现 {:?}", v))
    }
}
