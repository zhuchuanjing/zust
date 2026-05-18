use anyhow::{Result, anyhow, bail};
use compiler::{SymbolTable, eval_const_int_type};
use dynamic::{Dynamic, Type};
use smol_str::SmolStr;
use std::{collections::BTreeMap, rc::Rc};

#[derive(Debug, Clone)]
pub struct GpuStructLayout {
    pub name: SmolStr,
    pub ty: Type,
    pub size: usize,
    pub align: usize,
    pub fields: Vec<GpuFieldLayout>,
}

#[derive(Debug, Clone)]
pub struct GpuFieldLayout {
    pub name: SmolStr,
    pub ty: Type,
    pub offset: usize,
    pub size: usize,
    pub align: usize,
}

impl GpuStructLayout {
    pub(crate) fn from_symbol_table(symbols: &SymbolTable, name: &str, params: &[Type]) -> Result<Self> {
        let id = symbols.get_id(name)?;
        let ty = resolve_gpu_type(symbols, &Type::Symbol { id, params: params.to_vec() })?;
        Self::from_type(name.into(), ty)
    }

    pub(crate) fn from_type(name: SmolStr, ty: Type) -> Result<Self> {
        let Type::Struct { fields, .. } = &ty else {
            bail!("{name} is not a struct type: {ty:?}");
        };
        let (size, offsets) = Type::struct_layout(fields);
        let fields = fields.iter().zip(offsets).map(|((name, ty), offset)| GpuFieldLayout { name: name.clone(), ty: ty.clone(), offset: offset as usize, size: gpu_type_size(ty), align: gpu_type_align(ty) }).collect();
        let align = gpu_type_align(&ty);
        Ok(Self { name, ty, size: size as usize, align, fields })
    }

    pub fn pack_map(&self, value: &Dynamic) -> Result<Vec<u8>> {
        let mut bytes = vec![0; self.size];
        self.write_map(value, &mut bytes)?;
        Ok(bytes)
    }

    pub fn write_map(&self, value: &Dynamic, dst: &mut [u8]) -> Result<()> {
        if dst.len() < self.size {
            bail!("destination buffer too small for {}: need {}, got {}", self.name, self.size, dst.len());
        }
        write_value(&self.ty, value, &mut dst[..self.size])
    }

    pub fn unpack_map(&self, bytes: &[u8]) -> Result<Dynamic> {
        if bytes.len() < self.size {
            bail!("source buffer too small for {}: need {}, got {}", self.name, self.size, bytes.len());
        }
        read_value(&self.ty, &bytes[..self.size])
    }
}

pub(crate) fn resolve_gpu_type(symbols: &SymbolTable, ty: &Type) -> Result<Type> {
    let resolved = symbols.get_type(ty).unwrap_or_else(|_| ty.clone());
    match resolved {
        Type::Symbol { id, params } => {
            let params = params.iter().map(|param| resolve_gpu_type(symbols, param)).collect::<Result<Vec<_>>>()?;
            let resolved = symbols.get_type(&Type::Symbol { id, params })?;
            resolve_gpu_type(symbols, &resolved)
        }
        Type::Ident { name, params } => {
            let params = params.iter().map(|param| resolve_gpu_type(symbols, param)).collect::<Result<Vec<_>>>()?;
            let ident = Type::Ident { name, params };
            match symbols.get_type(&ident) {
                Ok(resolved) => resolve_gpu_type(symbols, &resolved),
                Err(_) => Ok(ident),
            }
        }
        Type::Struct { params, fields } => Ok(Type::Struct {
            params,
            fields: fields
                .iter()
                .filter_map(|(name, ty)| match resolve_gpu_type(symbols, ty) {
                    Ok(Type::Fn { .. }) => None,
                    Ok(ty) => Some(Ok((name.clone(), ty))),
                    Err(err) => Some(Err(err)),
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        Type::Vec(elem, len) => Ok(Type::Vec(Rc::new(resolve_gpu_type(symbols, &elem)?), len)),
        Type::Array(elem, len) => Ok(Type::Array(Rc::new(resolve_gpu_type(symbols, &elem)?), len)),
        Type::ArrayParam(elem, len) => {
            let elem = resolve_gpu_type(symbols, &elem)?;
            let len = resolve_gpu_type(symbols, &len)?;
            if let Some(len) = eval_const_int_type(&len) {
                let len = u32::try_from(len).map_err(|_| anyhow!("array length out of u32 range"))?;
                Ok(Type::Array(Rc::new(elem), len))
            } else {
                Ok(Type::ArrayParam(Rc::new(elem), Rc::new(len)))
            }
        }
        Type::Fn { tys, ret } => Ok(Type::Fn { tys: tys.iter().map(|ty| resolve_gpu_type(symbols, ty)).collect::<Result<Vec<_>>>()?, ret: Rc::new(resolve_gpu_type(symbols, &ret)?) }),
        Type::Tuple(items) => Ok(Type::Tuple(items.iter().map(|ty| resolve_gpu_type(symbols, ty)).collect::<Result<Vec<_>>>()?)),
        other => Ok(other),
    }
}

fn gpu_type_align(ty: &Type) -> usize {
    ty.align().min(8).max(1) as usize
}

fn gpu_type_size(ty: &Type) -> usize {
    ty.storage_width() as usize
}

fn write_value(ty: &Type, value: &Dynamic, dst: &mut [u8]) -> Result<()> {
    match ty {
        Type::Bool => write_scalar(dst, if value.is_true() { 1u8 } else { 0u8 }),
        Type::U8 => write_scalar(dst, u8::try_from(value.clone())?),
        Type::I8 => write_scalar(dst, i8::try_from(value.clone())?),
        Type::U16 => write_scalar(dst, u16::try_from(value.clone())?),
        Type::I16 => write_scalar(dst, i16::try_from(value.clone())?),
        Type::U32 => write_scalar(dst, u32::try_from(value.clone())?),
        Type::I32 => write_scalar(dst, i32::try_from(value.clone())?),
        Type::U64 => write_scalar(dst, u64::try_from(value.clone())?),
        Type::I64 => write_scalar(dst, i64::try_from(value.clone())?),
        Type::F32 => write_scalar(dst, f32::try_from(value.clone())?),
        Type::F64 => write_scalar(dst, f64::try_from(value.clone())?),
        Type::Struct { fields, .. } => {
            let (_, offsets) = Type::struct_layout(fields);
            for ((field_name, field_ty), offset) in fields.iter().zip(offsets) {
                let field_value = value.get_dynamic(field_name.as_str()).ok_or_else(|| anyhow!("missing struct field {field_name}"))?;
                let start = offset as usize;
                let end = start + gpu_type_size(field_ty);
                write_value(field_ty, &field_value, dst.get_mut(start..end).ok_or_else(|| anyhow!("field {field_name} out of buffer"))?)?;
            }
            Ok(())
        }
        Type::Array(elem, len) | Type::Vec(elem, len) if *len > 0 => write_sequence(elem, *len as usize, value, dst),
        Type::Tuple(items) => {
            for (idx, item_ty) in items.iter().enumerate() {
                let item = value.get_idx(idx).ok_or_else(|| anyhow!("missing tuple item {idx}"))?;
                let start = tuple_offset(items, idx);
                let end = start + gpu_type_size(item_ty);
                write_value(item_ty, &item, dst.get_mut(start..end).ok_or_else(|| anyhow!("tuple item {idx} out of buffer"))?)?;
            }
            Ok(())
        }
        Type::Void => Ok(()),
        other => bail!("unsupported GPU layout write type: {other:?}"),
    }
}

fn write_sequence(elem: &Type, len: usize, value: &Dynamic, dst: &mut [u8]) -> Result<()> {
    let stride = gpu_type_size(elem);
    if value.len() < len {
        bail!("sequence needs {len} items, got {}", value.len());
    }
    for idx in 0..len {
        let item = value.get_idx(idx).ok_or_else(|| anyhow!("missing sequence item {idx}"))?;
        let start = idx * stride;
        let end = start + stride;
        write_value(elem, &item, dst.get_mut(start..end).ok_or_else(|| anyhow!("sequence item {idx} out of buffer"))?)?;
    }
    Ok(())
}

fn read_value(ty: &Type, src: &[u8]) -> Result<Dynamic> {
    match ty {
        Type::Bool => Ok(Dynamic::Bool(read_scalar::<u8>(src)? != 0)),
        Type::U8 => Ok(Dynamic::U8(read_scalar(src)?)),
        Type::I8 => Ok(Dynamic::I8(read_scalar(src)?)),
        Type::U16 => Ok(Dynamic::U16(read_scalar(src)?)),
        Type::I16 => Ok(Dynamic::I16(read_scalar(src)?)),
        Type::U32 => Ok(Dynamic::U32(read_scalar(src)?)),
        Type::I32 => Ok(Dynamic::I32(read_scalar(src)?)),
        Type::U64 => Ok(Dynamic::U64(read_scalar(src)?)),
        Type::I64 => Ok(Dynamic::I64(read_scalar(src)?)),
        Type::F32 => Ok(Dynamic::F32(read_scalar(src)?)),
        Type::F64 => Ok(Dynamic::F64(read_scalar(src)?)),
        Type::Struct { fields, .. } => {
            let (_, offsets) = Type::struct_layout(fields);
            let mut map = BTreeMap::new();
            for ((field_name, field_ty), offset) in fields.iter().zip(offsets) {
                let start = offset as usize;
                let end = start + gpu_type_size(field_ty);
                let value = read_value(field_ty, src.get(start..end).ok_or_else(|| anyhow!("field {field_name} out of buffer"))?)?;
                map.insert(field_name.clone(), value);
            }
            Ok(Dynamic::map(map))
        }
        Type::Array(elem, len) | Type::Vec(elem, len) if *len > 0 => read_sequence(elem, *len as usize, src),
        Type::Tuple(items) => {
            let values = items
                .iter()
                .enumerate()
                .map(|(idx, item_ty)| {
                    let start = tuple_offset(items, idx);
                    let end = start + gpu_type_size(item_ty);
                    read_value(item_ty, src.get(start..end).ok_or_else(|| anyhow!("tuple item {idx} out of buffer"))?)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Dynamic::list(values))
        }
        Type::Void => Ok(Dynamic::Null),
        other => bail!("unsupported GPU layout read type: {other:?}"),
    }
}

fn read_sequence(elem: &Type, len: usize, src: &[u8]) -> Result<Dynamic> {
    let stride = gpu_type_size(elem);
    let values = (0..len)
        .map(|idx| {
            let start = idx * stride;
            let end = start + stride;
            read_value(elem, src.get(start..end).ok_or_else(|| anyhow!("sequence item {idx} out of buffer"))?)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Dynamic::list(values))
}

fn tuple_offset(items: &[Type], idx: usize) -> usize {
    items.iter().take(idx).map(gpu_type_size).sum()
}

fn write_scalar<T: bytemuck::NoUninit>(dst: &mut [u8], value: T) -> Result<()> {
    let bytes = bytemuck::bytes_of(&value);
    let target = dst.get_mut(..bytes.len()).ok_or_else(|| anyhow!("buffer too small for scalar"))?;
    target.copy_from_slice(bytes);
    Ok(())
}

fn read_scalar<T: bytemuck::AnyBitPattern>(src: &[u8]) -> Result<T> {
    let size = std::mem::size_of::<T>();
    let bytes = src.get(..size).ok_or_else(|| anyhow!("buffer too small for scalar"))?;
    Ok(bytemuck::pod_read_unaligned(bytes))
}
