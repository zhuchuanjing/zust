use dynamic::{ConstIntOp, Dynamic, Type};
use parser::Stmt;
use smol_str::SmolStr;
use std::{rc::Rc, sync::Arc};

use super::Capture;

#[derive(Debug, Clone, Default)]
pub enum Symbol {
    #[default]
    Null,
    Const {
        value: Dynamic,
        ty: Type,
        is_pub: bool,
    },
    Static {
        value: Option<Dynamic>,
        ty: Type,
        is_pub: bool,
    },
    Struct(Type, bool),
    Fn {
        ty: Type,
        args: Vec<SmolStr>,
        generic_params: Vec<Type>,
        cap: Capture,
        body: Arc<Stmt>,
        is_pub: bool,
    },
    Native(Type),
}

impl Symbol {
    pub fn native(tys: Vec<Type>, ret: Type) -> Self {
        Self::Native(Type::Fn { tys, ret: Rc::new(ret) })
    }

    pub fn is_pub(&self) -> bool {
        match self {
            Self::Const { value: _, ty: _, is_pub } => *is_pub,
            Self::Static { value: _, ty: _, is_pub } => *is_pub,
            Self::Struct(_, is_pub) => *is_pub,
            Self::Fn { ty: _, args: _, generic_params: _, cap: _, body: _, is_pub } => *is_pub,
            _ => true,
        }
    }

    pub fn is_fn(&self) -> bool {
        match self {
            Self::Fn { ty: _, args: _, generic_params: _, cap: _, body: _, is_pub: _ } => true,
            Self::Native(_) => true,
            _ => false,
        }
    }
}

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use std::{cell::RefCell, collections::HashMap};

pub fn eval_const_int_type(ty: &Type) -> Option<i64> {
    match ty {
        Type::ConstInt(value) => Some(*value),
        Type::ConstBinary { op, left, right } => {
            let left = eval_const_int_type(left)?;
            let right = eval_const_int_type(right)?;
            // 用 checked 算术:溢出时返回 None(放弃常量折叠,回退到 ArrayParam),
            // 而非静默回绕成错误长度(如 i64::MAX+1 -> i64::MIN 可能落在合法 u32 范围)。
            match op {
                ConstIntOp::Add => left.checked_add(right),
                ConstIntOp::Sub => left.checked_sub(right),
                ConstIntOp::Mul => left.checked_mul(right),
                ConstIntOp::Div => (right != 0).then(|| left / right),
                ConstIntOp::Mod => (right != 0).then(|| left % right),
            }
        }
        _ => None,
    }
}

pub fn substitute_type(ty: &Type, params: &[Type], args: &[Type]) -> Type {
    match ty {
        Type::Ident { name, params: nested } if nested.is_empty() => {
            params.iter().position(|param| matches!(param, Type::Ident { name: param_name, params } if params.is_empty() && param_name == name)).map(|idx| args[idx].clone()).unwrap_or_else(|| ty.clone())
        }
        Type::Ident { name, params: nested } => Type::Ident { name: name.clone(), params: nested.iter().map(|param| substitute_type(param, params, args)).collect() },
        Type::Struct { params: struct_params, fields } => Type::Struct {
            params: struct_params.iter().map(|param| substitute_type(param, params, args)).collect(),
            fields: fields.iter().map(|(name, field_ty)| (name.clone(), substitute_type(field_ty, params, args))).collect(),
        },
        Type::List(elem) => Type::List(Rc::new(substitute_type(elem, params, args))),
        Type::Vec(elem, len) => Type::Vec(Rc::new(substitute_type(elem, params, args)), *len),
        Type::Array(elem, len) => Type::Array(Rc::new(substitute_type(elem, params, args)), *len),
        Type::ArrayParam(elem, len) => Type::ArrayParam(Rc::new(substitute_type(elem, params, args)), Rc::new(substitute_type(len, params, args))),
        Type::ConstBinary { op, left, right } => {
            let left = substitute_type(left, params, args);
            let right = substitute_type(right, params, args);
            let ty = Type::ConstBinary { op: *op, left: Rc::new(left), right: Rc::new(right) };
            eval_const_int_type(&ty).map(Type::ConstInt).unwrap_or(ty)
        }
        Type::Fn { tys, ret } => Type::Fn { tys: tys.iter().map(|ty| substitute_type(ty, params, args)).collect(), ret: Rc::new(substitute_type(ret, params, args)) },
        Type::Symbol { id, params: nested } => Type::Symbol { id: *id, params: nested.iter().map(|param| substitute_type(param, params, args)).collect() },
        Type::Tuple(items) => Type::Tuple(items.iter().map(|item| substitute_type(item, params, args)).collect()),
        _ => ty.clone(),
    }
}

#[derive(Clone, Default)]
pub struct SymbolTable {
    pub symbols: IndexMap<SmolStr, Symbol>,
    // 双 IndexMap:按模块名 + 模块内符号名 O(1) 查找;按插入顺序遍历。
    modules: IndexMap<SmolStr, IndexMap<SmolStr, u32>>,
    pub roots: Vec<SmolStr>,
    lookup_cache: RefCell<HashMap<SmolStr, (u64, u32)>>,
    lookup_epoch: u64,
}

impl SymbolTable {
    fn invalidate_lookup_cache(&mut self) {
        self.lookup_epoch = self.lookup_epoch.wrapping_add(1);
        self.lookup_cache.get_mut().clear();
    }

    fn cached_id(&self, name: &str) -> Option<u32> {
        let key = SmolStr::new(name);
        self.lookup_cache.borrow().get(&key).and_then(|(epoch, id)| (*epoch == self.lookup_epoch).then_some(*id))
    }

    fn cache_id(&self, name: &str, id: u32) {
        self.lookup_cache.borrow_mut().insert(SmolStr::new(name), (self.lookup_epoch, id));
    }

    pub fn add_to_module(&mut self, module: &str, name: SmolStr, s: Symbol) -> Result<u32> {
        self.invalidate_lookup_cache();
        let full_name: SmolStr = format!("{}::{}", module, name).into();
        let id = self.symbols.insert_full(full_name, s).0 as u32;
        let module_symbols = self.modules.get_mut(module).ok_or_else(|| anyhow!("模块 {} 不存在", module))?;
        module_symbols.insert(name, id);
        Ok(id)
    }
    pub fn get_symbol(&self, idx: u32) -> Result<(&SmolStr, &Symbol)> {
        self.symbols.get_index(idx as usize).ok_or(anyhow!("未发现符号 {}", idx))
    }

    pub fn get_symbol_mut(&mut self, idx: u32) -> Option<(&SmolStr, &mut Symbol)> {
        self.symbols.get_index_mut(idx as usize)
    }

    pub fn symbol(&self, name: &str) -> Vec<(SmolStr, u32)> {
        self.modules.get(name).map(|m| m.iter().map(|(name, id)| (name.clone(), *id)).collect()).unwrap_or(Vec::new())
    }

    pub fn disassemble(&self, name: &str) -> Result<String> {
        let id = self.get_id(name)?;
        let (name, s) = self.get_symbol(id)?;
        if let Symbol::Fn { ty, args, generic_params: _, cap, body, is_pub } = s {
            if *is_pub { Ok(format!("pub {} {:?} {:?} {:?}\n{}", name, ty, args, cap, body)) } else { Ok(format!("{} {:?} {:?} {:?}\n{}", name, ty, args, cap, body)) }
        } else {
            Err(anyhow!("未发现符号 {}", name))
        }
    }

    pub fn get_field(&self, ty: &Type, name: &str) -> Result<(usize, Type)> {
        // 这些转换/复制方法适用于所有值。统一经 Any 原语实现，避免整数、Bool、
        // 字符串等静态类型各自重复暴露一套同义方法。
        if matches!(name, "clone" | "to_string" | "to_int" | "parse_int" | "to_number" | "parse_number" | "to_json") {
            return Ok((usize::MAX, Type::Symbol { id: self.get_id(&format!("Any::{}", name))?, params: Vec::new() }));
        }
        //原生类型的函数 is_map is_list 或者 sqrt
        let id = match ty {
            Type::Any => {
                if let Ok(id) = self.get_id("Any")
                    && let Ok((_, Symbol::Struct(any_ty, _))) = self.get_symbol(id)
                    && let Ok((idx, field_ty)) = any_ty.get_field(name)
                {
                    return Ok((idx, field_ty.clone()));
                }
                match name {
                    "is_map" | "is_list" | "is_string" | "is_null" | "is_some" | "is_none" | "is_number" | "is_integer" | "is_bool" | "is_int" | "is_float" | "is_empty" | "contains" | "starts_with" | "ends_with" => return Ok((usize::MAX, Type::Bool)),
                    "len" | "length" => return Ok((usize::MAX, Type::I32)),
                    "byte_len" => return Ok((usize::MAX, Type::I64)),
                    _ => return Ok((usize::MAX, Type::Any)),
                }
            }
            Type::Bool | Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::F16 | Type::F32 | Type::F64 => {
                // 标量类型只暴露类型自检方法（is_bool/is_int/is_float）与 Any 转换
                let any_method = match name {
                    "is_bool" | "is_int" | "is_float" | "is_map" | "is_list" | "is_string" | "is_null" | "is_some" | "is_none" | "is_number" | "is_integer" | "is_empty" => {
                        format!("Any::{}", name)
                    }
                    _ => return Err(anyhow!("未发现 symbol {:?} {}", ty, name)),
                };
                return Ok((usize::MAX, Type::Symbol { id: self.get_id(&any_method)?, params: Vec::new() }));
            }
            Type::Struct { params: _, fields: _ } => {
                return ty.get_field(name).map(|(idx, ty)| (idx, ty.clone()));
            }
            Type::Str => {
                let any_method = match name {
                    "len" | "length" | "is_empty" | "contains" | "split" | "starts_with" | "ends_with" | "is_string" | "is_null" | "is_some" | "is_none" | "is_bool" | "is_int" | "is_float" | "trim" | "trim_start" | "trim_end" | "trim_matches" | "trim_start_matches"
                    | "trim_end_matches" | "find" | "rfind" | "replace" | "replace_all" | "to_lower" | "to_upper" | "substring" | "byte_len" | "byte_slice" | "from_yaml" | "to_yaml" | "from_json" => {
                        format!("Any::{}", name)
                    }
                    _ => return Err(anyhow!("未发现 symbol {:?} {}", ty, name)),
                };
                return Ok((usize::MAX, Type::Symbol { id: self.get_id(&any_method)?, params: Vec::new() }));
            }
            Type::List(_) | Type::Array(_, _) => {
                let any_method = match name {
                    "append" | "add" => "Any::push".to_string(),
                    "get" => "Any::get_idx".to_string(),
                    "set" => "Any::set_idx".to_string(),
                    "contains_key" => "Any::contains".to_string(),
                    "len" | "length" | "is_empty" | "push" | "pop" | "insert" | "get_idx" | "set_idx" | "slice" | "contains" | "is_list" | "is_null" | "is_some" | "is_none" | "join" | "sort" | "to_yaml" | "from_yaml" => {
                        format!("Any::{}", name)
                    }
                    _ => return Err(anyhow!("未发现 symbol {:?} {}", ty, name)),
                };
                return Ok((usize::MAX, Type::Symbol { id: self.get_id(&any_method)?, params: Vec::new() }));
            }
            Type::Map => {
                let any_method = match name {
                    "contains_key" => "Any::contains".to_string(),
                    "len" | "length" | "is_empty" | "keys" | "contains" | "get" | "get_key" | "set" | "set_key" | "del_key" | "is_map" | "is_null" | "is_some" | "is_none" | "to_yaml" | "from_yaml" => format!("Any::{}", name),
                    _ => return Err(anyhow!("未发现 symbol {:?} {}", ty, name)),
                };
                return Ok((usize::MAX, Type::Symbol { id: self.get_id(&any_method)?, params: Vec::new() }));
            }
            Type::Symbol { id, params: _ } => *id,
            Type::Vec(_, _) => self.get_id("Vec")?,
            Type::Fn { tys: _, ret } => {
                return self.get_field(ret, name);
            }
            _ => {
                //增加一个外部函数定义
                if matches!(name, "is_map" | "is_list" | "is_string" | "is_null") {
                    return Ok((usize::MAX, Type::Symbol { id: self.get_id(&format!("Any::{}", name))?, params: Vec::new() }));
                }
                return Err(anyhow!("未发现 symbol {:?} {}", ty, name));
            }
        };
        let (_, s) = self.get_symbol(id)?;
        if let Symbol::Struct(s, _) = s {
            return s.get_field(name).and_then(|(idx, ty)| Ok((idx, ty.clone())));
        };
        Err(anyhow!("未发现 field {:?} {}", ty, name))
    }

    pub fn get_type(&self, ty: &Type) -> Result<Type> {
        match ty {
            Type::Ident { name, params } => {
                let params = params.iter().map(|param| self.get_type(param)).collect::<Result<Vec<_>>>()?;
                if name.as_str() == "Vec" && params.len() == 1 {
                    return Ok(Type::Vec(Rc::new(params[0].clone()), 0));
                }
                if name.as_str() == "List" {
                    return Ok(if params.is_empty() { Type::list_any() } else { Type::List(Rc::new(params[0].clone())) });
                }
                let id = self.get_id(&name)?;
                if let (_, Symbol::Struct(ty, _)) = self.get_symbol(id)? {
                    if let Type::Struct { params: generic_params, .. } = ty
                        && !generic_params.is_empty()
                        && generic_params.len() == params.len()
                    {
                        return self.get_type(&substitute_type(ty, generic_params, &params));
                    }
                    return self.get_type(ty);
                }
                return Ok(Type::Symbol { id, params });
            }
            Type::Symbol { id, params } => {
                return match self.get_symbol(*id)? {
                    (_, Symbol::Fn { ty, args: _, generic_params: _, cap: _, body: _, is_pub: _ }) => self.get_type(ty),
                    (_, Symbol::Native(ty)) => self.get_type(ty),
                    (_, Symbol::Struct(ty, _)) => {
                        let params = params.iter().map(|param| self.get_type(param)).collect::<Result<Vec<_>>>()?;
                        if let Type::Struct { params: generic_params, .. } = ty
                            && !generic_params.is_empty()
                            && generic_params.len() == params.len()
                        {
                            self.get_type(&substitute_type(ty, generic_params, &params))
                        } else {
                            self.get_type(ty)
                        }
                    }
                    (_, s) => {
                        log::debug!("s-> {:?}", s);
                        Ok(Type::Symbol { id: *id, params: params.clone() })
                    }
                };
            }
            Type::Vec(elem, len) => {
                return Ok(Type::Vec(Rc::new(self.get_type(elem)?), *len));
            }
            Type::List(elem) => {
                return Ok(Type::List(Rc::new(self.get_type(elem)?)));
            }
            Type::Array(elem, len) => {
                return Ok(Type::Array(Rc::new(self.get_type(elem)?), *len));
            }
            Type::ArrayParam(elem, len) => {
                let elem = self.get_type(elem)?;
                let len = self.get_type(len)?;
                if let Some(len) = eval_const_int_type(&len) {
                    let len = u32::try_from(len).map_err(|_| anyhow!("数组长度超出 u32 范围"))?;
                    return Ok(Type::Array(Rc::new(elem), len));
                }
                return Ok(Type::ArrayParam(Rc::new(elem), Rc::new(len)));
            }
            Type::ConstBinary { op, left, right } => {
                let left = self.get_type(left)?;
                let right = self.get_type(right)?;
                let ty = Type::ConstBinary { op: *op, left: Rc::new(left), right: Rc::new(right) };
                return Ok(eval_const_int_type(&ty).map(Type::ConstInt).unwrap_or(ty));
            }
            Type::Fn { tys, ret } => {
                return Ok(Type::Fn { tys: tys.iter().map(|ty| self.get_type(ty)).collect::<Result<Vec<_>>>()?, ret: Rc::new(self.get_type(ret)?) });
            }
            Type::Struct { params, fields } => {
                return Ok(Type::Struct {
                    params: params.iter().map(|param| self.get_type(param)).collect::<Result<Vec<_>>>()?,
                    fields: fields.iter().map(|(name, ty)| if matches!(ty, Type::Symbol { .. }) { Ok((name.clone(), ty.clone())) } else { self.get_type(ty).map(|ty| (name.clone(), ty)) }).collect::<Result<Vec<_>>>()?,
                });
            }
            _ => {}
        }
        Ok(ty.clone())
    }

    pub fn add_module(&mut self, name: SmolStr) {
        self.invalidate_lookup_cache();
        let len = self.roots.len();
        if let Some(pos) = self.roots.iter().position(|r| r.as_str() == name.as_str()) {
            if pos != len - 1 {
                self.roots.swap(pos, len - 1);
            }
        } else {
            self.roots.push(name.clone());
        }
        self.modules.insert(name, IndexMap::new());
    }

    pub fn push_module_scope(&mut self, name: SmolStr) {
        self.invalidate_lookup_cache();
        self.roots.push(name);
    }

    pub fn pop_module_scope(&mut self) {
        self.invalidate_lookup_cache();
        self.roots.pop();
    }

    pub fn pop_module(&mut self) {
        self.invalidate_lookup_cache();
        //如果不想模块成为全局的 add_module 之后调用 pop_module
        if let Some(last) = self.roots.pop() {
            if let Some(names) = self.modules.get(&last).map(|m| {
                let kvs: Vec<(SmolStr, u32)> = m.iter().map(|kv| (kv.0.clone(), *kv.1)).collect();
                kvs.iter().filter_map(|kv| if !self.get_symbol(kv.1).map(|s| s.1.is_pub()).unwrap_or(false) { Some(kv.0.clone()) } else { None }).collect::<Vec<_>>()
            }) {
                if let Some(m) = self.modules.get_mut(&last) {
                    for name in names {
                        m.shift_remove(&name); //删除非 pub 的符号;保持插入顺序
                    }
                }
            }
        }
    }

    pub fn get_id(&self, name: &str) -> Result<u32> {
        if let Some(id) = self.cached_id(name) {
            return Ok(id);
        }
        // 1) 全名命中 (含 :: 的 id 或 add_global 注册过的全局符号)。
        if let Some(idx) = self.symbols.get_index_of(name) {
            let id = idx as u32;
            self.cache_id(name, id);
            return Ok(id);
        }
        // 2) 含 :: 的名字拆分按模块查找,O(1)。
        if let Some((mod_name, sym_name)) = name.split_once("::") {
            if let Some(&id) = self.modules.get(mod_name).and_then(|m| m.get(sym_name)) {
                self.cache_id(name, id);
                return Ok(id);
            }
            // 即使 modules 里被 pop_module 移除,完整名仍可能在 symbols IndexMap 里。
            let full = format!("{mod_name}::{sym_name}");
            if let Some(idx) = self.symbols.get_index_of(full.as_str()) {
                let id = idx as u32;
                self.cache_id(name, id);
                return Ok(id);
            }
        }
        // 3) 不含 :: 的短名,按 roots 倒序查找第一个匹配的模块。O(M)。
        //    pop_module 会清掉非 pub 项,所以 modules 可能没有;再退回按 root 拼全名查 symbols。
        let short_name = name;
        for root in self.roots.iter().rev() {
            if let Some(&id) = self.modules.get(root).and_then(|m| m.get(short_name)) {
                self.cache_id(name, id);
                return Ok(id);
            }
            let full = format!("{root}::{short_name}");
            if let Some(idx) = self.symbols.get_index_of(full.as_str()) {
                let id = idx as u32;
                self.cache_id(name, id);
                return Ok(id);
            }
        }
        // 4) 兜底:任意模块下的同名短符号。O(M*K),K 为模块内符号数。
        for (_, m) in &self.modules {
            if let Some(&id) = m.get(short_name) {
                self.cache_id(name, id);
                return Ok(id);
            }
        }
        Err(anyhow!("{} 未发现", name))
    }

    pub fn add(&mut self, name: SmolStr, s: Symbol) -> u32 {
        self.invalidate_lookup_cache();
        let root = self.roots.last().cloned().unwrap();
        let id = self.symbols.insert_full(format!("{}::{}", root, name).into(), s).0 as u32;
        self.modules.get_mut(&root).map(|m| m.insert(name, id));
        id
    }

    pub fn add_global(&mut self, name: SmolStr, s: Symbol) -> u32 {
        if let Some(idx) = self.symbols.get_index_of(name.as_str()) {
            return idx as u32;
        }
        if let Some((mod_name, symbol_name)) = name.as_str().split_once("::") {
            if let Some(m) = self.modules.get_mut(mod_name) {
                if let Some(&id) = m.get(symbol_name) {
                    return id;
                }
            }
        }
        self.invalidate_lookup_cache();
        let id = self.symbols.insert_full(name.clone(), s).0 as u32;
        if let Some((mod_name, symbol_name)) = name.as_str().split_once("::") {
            if let Some(m) = self.modules.get_mut(mod_name) {
                m.insert(symbol_name.into(), id);
            }
        }
        id
    }

    pub fn take(&mut self, id: u32) -> Option<Symbol> {
        self.invalidate_lookup_cache();
        self.symbols.get_index_mut(id as usize).map(|(_, s)| std::mem::take(s))
    }
}
