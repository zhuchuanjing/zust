use dynamic::Type;
use rspirv::dr::Operand;
use spirv::{Capability, Decoration};
use std::rc::Rc;

use crate::context::{SpirvCompiler, SpirvTy};

impl SpirvCompiler {
    pub(crate) fn resolve_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Symbol { id, .. } => self.type_defs.get(id).cloned().unwrap_or_else(|| ty.clone()),
            Type::Struct { params, fields } => Type::Struct {
                params: params.iter().map(|ty| self.resolve_type(ty)).collect(),
                fields: fields.iter().filter_map(|(name, ty)| if matches!(ty, Type::Symbol { id, .. } if !self.type_defs.contains_key(id)) { None } else { Some((name.clone(), self.resolve_type(ty))) }).collect(),
            },
            Type::Vec(elem, len) => Type::Vec(Rc::new(self.resolve_type(elem)), *len),
            Type::Array(elem, len) => Type::Array(Rc::new(self.resolve_type(elem)), *len),
            Type::Fn { tys, ret } => Type::Fn { tys: tys.iter().map(|ty| self.resolve_type(ty)).collect(), ret: Rc::new(self.resolve_type(ret)) },
            _ => ty.clone(),
        }
    }

    pub(crate) fn get_type(&mut self, ty: SpirvTy) -> u32 {
        let ty = match ty {
            SpirvTy::Value(value) => SpirvTy::Value(self.resolve_type(&value)),
            SpirvTy::LayoutValue(value) => SpirvTy::LayoutValue(self.resolve_type(&value)),
            SpirvTy::Pointer(value, storage) => SpirvTy::Pointer(self.resolve_type(&value), storage),
            SpirvTy::Buffer(value) => SpirvTy::Buffer(self.resolve_type(&value)),
        };
        if let Some((_, id)) = self.types.iter().find(|(existing, _)| existing == &ty) {
            return *id;
        }
        let id = match ty.clone() {
            SpirvTy::Value(Type::Void) => self.builder.type_void(),
            SpirvTy::Value(Type::Bool) => self.builder.type_bool(),
            SpirvTy::Value(t) if t.is_int() => {
                self.enable_int_capability(&t);
                self.builder.type_int(t.width() * 8, 1)
            }
            SpirvTy::Value(t) if t.is_uint() => {
                self.enable_int_capability(&t);
                self.builder.type_int(t.width() * 8, 0)
            }
            SpirvTy::Value(t) if t.is_float() => {
                if t.is_f64() {
                    self.builder.capability(Capability::Float64);
                }
                self.builder.type_float(t.width() * 8, None)
            }
            SpirvTy::Value(Type::Vec(elem, len)) => {
                assert!(len > 0, "runtime arrays are only valid as storage buffers");
                let elem = self.get_type(SpirvTy::Value((*elem).clone()));
                self.builder.type_vector(elem, len)
            }
            SpirvTy::Value(Type::Array(elem, len)) => {
                let elem = self.get_type(SpirvTy::Value((*elem).clone()));
                let len = self.const_u32(len);
                self.builder.type_array(elem, len)
            }
            SpirvTy::Value(Type::Struct { params: _, fields }) => {
                let ids = fields.iter().map(|(_, ty)| self.get_type(SpirvTy::Value(ty.clone()))).collect::<Vec<_>>();
                self.builder.type_struct(ids)
            }
            SpirvTy::LayoutValue(Type::Array(elem, len)) => {
                let elem_ty = (*elem).clone();
                let elem = self.get_type(SpirvTy::LayoutValue(elem_ty.clone()));
                let len = self.const_u32(len);
                let arr_id = self.builder.type_array(elem, len);
                self.builder.decorate(arr_id, Decoration::ArrayStride, [Operand::LiteralBit32(elem_ty.storage_width())]);
                arr_id
            }
            SpirvTy::LayoutValue(Type::Struct { params: _, fields }) => {
                let ids = fields.iter().map(|(_, ty)| self.get_type(SpirvTy::LayoutValue(ty.clone()))).collect::<Vec<_>>();
                let struct_id = self.builder.type_struct(ids);
                let (_, offsets) = Type::struct_layout(&fields);
                for (idx, offset) in offsets.into_iter().enumerate() {
                    self.builder.member_decorate(struct_id, idx as u32, Decoration::Offset, [Operand::LiteralBit32(offset)]);
                }
                struct_id
            }
            SpirvTy::LayoutValue(value) => self.get_type(SpirvTy::Value(value)),
            SpirvTy::Pointer(value, storage @ spirv::StorageClass::StorageBuffer) => {
                let value = self.get_type(SpirvTy::LayoutValue(value));
                self.builder.type_pointer(None, storage, value)
            }
            SpirvTy::Pointer(value, storage) => {
                let value = self.get_type(SpirvTy::Value(value));
                self.builder.type_pointer(None, storage, value)
            }
            SpirvTy::Buffer(value) => {
                let struct_id = if let Type::Vec(elem_ty, 0) = value.clone() {
                    let elem_id = self.get_type(SpirvTy::Value((*elem_ty).clone()));
                    let arr_id = self.builder.type_runtime_array(elem_id);
                    self.builder.decorate(arr_id, Decoration::ArrayStride, [Operand::LiteralBit32(elem_ty.storage_width())]);
                    self.builder.type_struct([arr_id])
                } else {
                    let value_ty = self.get_type(SpirvTy::LayoutValue(value.clone()));
                    self.builder.type_struct([value_ty])
                };
                self.builder.decorate(struct_id, Decoration::Block, []);
                self.builder.member_decorate(struct_id, 0, Decoration::Offset, [Operand::LiteralBit32(0)]);
                struct_id
            }
            other => panic!("unsupported SPIR-V type: {other:?}"),
        };
        self.types.push((ty, id));
        id
    }

    pub(crate) fn glsl_import(&mut self) -> u32 {
        self.builder.ext_inst_import("GLSL.std.450")
    }

    pub(crate) fn enable_int_capability(&mut self, ty: &Type) {
        match ty.width() {
            1 => self.builder.capability(Capability::Int8),
            2 => self.builder.capability(Capability::Int16),
            8 => self.builder.capability(Capability::Int64),
            _ => {}
        }
    }
}
