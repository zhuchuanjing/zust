pub trait MsgPack {
    fn encode(&self, buf: &mut Vec<u8>);
}

use indexmap::IndexMap;

use byteorder::{BigEndian, WriteBytesExt};
use smol_str::SmolStr;
impl MsgPack for &str {
    fn encode(&self, buf: &mut Vec<u8>) {
        let length = self.len();
        if length < 0x20 {
            buf.push(0xa0 | length as u8);
        } else if length < 0x100 {
            buf.push(0xd9);
            buf.push(length as u8);
        } else if length < 0x10000 {
            buf.push(0xda);
            buf.write_u16::<BigEndian>(length as u16).unwrap();
        } else {
            buf.push(0xdb);
            buf.write_u32::<BigEndian>(length as u32).unwrap();
        }
        buf.extend_from_slice(self.as_bytes());
    }
}

impl MsgPack for i64 {
    fn encode(&self, buf: &mut Vec<u8>) {
        let value = *self;
        if value >= 0 && value < 128 {
            buf.push(value as u8);
        } else if value < 0 && value > -32 {
            let raw = (value as i8) as u8;
            buf.push(raw);
        } else {
            if value >= -0x80 && value <= 0x7f {
                buf.push(0xd0);
                buf.write_i8(value as i8).unwrap();
            } else if value >= -0x8000 && value <= 0x7fff {
                buf.push(0xd1);
                buf.write_i16::<BigEndian>(value as i16).unwrap();
            } else if value >= -0x8000_0000 && value <= 0x7fff_ffff {
                buf.push(0xd2);
                buf.write_i32::<BigEndian>(value as i32).unwrap();
            } else {
                buf.push(0xd3);
                buf.write_i64::<BigEndian>(value).unwrap();
            }
        }
    }
}

use super::Dynamic;

fn encode_bytes(raw: &[u8], buf: &mut Vec<u8>) {
    let length = raw.len();
    if length < 0x100 {
        buf.push(0xc4);
        buf.push(length as u8);
    } else if length < 0x10000 {
        buf.push(0xc5);
        buf.write_u16::<BigEndian>(length as u16).unwrap();
    } else {
        buf.push(0xc6);
        buf.write_u32::<BigEndian>(length as u32).unwrap();
    }
    buf.extend_from_slice(&raw);
}

fn encode_vec(raw: &[u8], len: usize, tag: u8, buf: &mut Vec<u8>) {
    if len < 0x100 {
        buf.push(0xc7);
        buf.push(len as u8);
        buf.push(tag);
    } else if len < 0x10000 {
        buf.push(0xc8);
        buf.write_u16::<BigEndian>(len as u16).unwrap();
        buf.push(tag);
    } else {
        buf.push(0xc9);
        buf.write_u32::<BigEndian>(len as u32).unwrap();
        buf.push(tag);
    }
    buf.extend_from_slice(raw);
}

impl MsgPack for Dynamic {
    fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            Dynamic::Iter { idx: _, keys: _, value: _ } => {}
            Dynamic::Null => buf.push(0xc0),
            Dynamic::Bool(b) => buf.push(if *b { 0xc3 } else { 0xc2 }),
            Dynamic::I8(i) => (*i as i64).encode(buf),
            Dynamic::I16(i) => (*i as i64).encode(buf),
            Dynamic::I32(i) => (*i as i64).encode(buf),
            Dynamic::I64(i) => (*i).encode(buf),
            Dynamic::U8(i) => (*i as i64).encode(buf),
            Dynamic::U16(i) => (*i as i64).encode(buf),
            Dynamic::U32(i) => (*i as i64).encode(buf),
            Dynamic::U64(i) => (*i as i64).encode(buf),
            Dynamic::F16(bits) => {
                // round-trip via f64 for msgpack; native f16 has no msgpack tag
                let f = crate::f16_to_f64(*bits);
                Dynamic::F64(f).encode(buf);
            }
            Dynamic::F32(f) => {
                buf.push(0xca);
                let int_value = f32::to_bits(*f);
                buf.write_u32::<BigEndian>(int_value).unwrap();
            }
            Dynamic::F64(f) => {
                buf.push(0xcb);
                let int_value = f64::to_bits(*f);
                buf.write_u64::<BigEndian>(int_value).unwrap();
            }
            Dynamic::String(s) => s.as_str().encode(buf),
            Dynamic::StringBuf(s) => s.as_str().encode(buf),
            Dynamic::Bytes(raw) => {
                encode_bytes(raw.as_slice(), buf);
            }
            Dynamic::VecI8(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 1, buf);
            }
            Dynamic::VecU16(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 2, buf);
            }
            Dynamic::VecI16(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 3, buf);
            }
            Dynamic::VecU32(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 4, buf);
            }
            Dynamic::VecI32(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 5, buf);
            }
            Dynamic::VecF32(vec) => {
                let len = self.len();
                encode_vec(vec.as_slice(), len, 6, buf);
            }
            Dynamic::VecU64(vec) => {
                let len = self.len();
                encode_vec(bytemuck::cast_slice(vec.as_slice()), len, 7, buf);
            }
            Dynamic::VecI64(vec) => {
                let len = self.len();
                encode_vec(bytemuck::cast_slice(vec.as_slice()), len, 8, buf);
            }
            Dynamic::VecF64(vec) => {
                let len = self.len();
                encode_vec(bytemuck::cast_slice(vec.as_slice()), len, 9, buf);
            }
            Dynamic::List(raw) => {
                let length = raw.read().len();
                if length < 0x10 {
                    buf.push(0x90 | length as u8);
                } else if length < 0x10000 {
                    buf.push(0xdc);
                    buf.write_u16::<BigEndian>(length as u16).unwrap();
                } else {
                    buf.push(0xdd);
                    buf.write_u32::<BigEndian>(length as u32).unwrap();
                }
                for item in raw.read().iter() {
                    item.encode(buf);
                }
            }
            Dynamic::Map(raw) => {
                let length = raw.read().len();
                if length < 16 {
                    buf.push(0x80 | length as u8);
                } else if length <= 0x10000 {
                    buf.push(0xde);
                    buf.write_u16::<BigEndian>(length as u16).unwrap();
                } else {
                    buf.push(0xdf);
                    buf.write_u32::<BigEndian>(length as u32).unwrap();
                }
                for (k, v) in raw.read().iter() {
                    k.as_str().encode(buf);
                    v.encode(buf);
                }
            }
            Dynamic::Struct { .. } => {
                let keys = self.keys();
                let length = keys.len();
                if length < 16 {
                    buf.push(0x80 | length as u8);
                } else if length <= 0x10000 {
                    buf.push(0xde);
                    buf.write_u16::<BigEndian>(length as u16).unwrap();
                } else {
                    buf.push(0xdf);
                    buf.write_u32::<BigEndian>(length as u32).unwrap();
                }
                for key in keys {
                    key.as_str().encode(buf);
                    self.get_dynamic(key.as_str()).unwrap_or(Dynamic::Null).encode(buf);
                }
            }
            Dynamic::Custom(value) => {
                buf.push(0x81);
                "@custom".encode(buf);
                value.custom_type_name().encode(buf);
            }
        }
    }
}
use anyhow::{Result, anyhow};

pub trait MsgUnpack: Sized {
    fn decode(buf: &[u8]) -> Result<(Self, usize)>;
    fn decode_array(buf: &[u8], length: usize) -> Result<(Vec<Self>, usize)> {
        let mut cursor = 0usize;
        let mut result = Vec::with_capacity(length);
        for _ in 0..length {
            let (value, size) = Self::decode(&buf[cursor..])?;
            result.push(value);
            cursor += size;
        }
        Ok((result, cursor))
    }
}

#[inline]
pub(crate) fn read_8(raw: &[u8]) -> u8 {
    raw[0]
}

#[inline]
pub(crate) fn read_16(raw: &[u8]) -> u16 {
    raw[1] as u16 | (raw[0] as u16) << 8
}

#[inline]
pub(crate) fn read_32(raw: &[u8]) -> u32 {
    raw[3] as u32 | (raw[2] as u32) << 8 | (raw[1] as u32) << 16 | (raw[0] as u32) << 24
}

#[inline]
pub(crate) fn read_64(raw: &[u8]) -> u64 {
    raw[7] as u64 | (raw[6] as u64) << 8 | (raw[5] as u64) << 16 | (raw[4] as u64) << 24 | (raw[3] as u64) << 32 | (raw[2] as u64) << 40 | (raw[1] as u64) << 48 | (raw[0] as u64) << 56
}

fn vec_to_map(kvs: Vec<Dynamic>) -> Result<Dynamic> {
    let mut map: IndexMap<SmolStr, Dynamic> = IndexMap::new();
    let mut key: Option<Dynamic> = None;
    for kv in kvs {
        if let Some(k) = key.take() {
            map.insert(SmolStr::from(k.to_string()), kv);
        } else {
            key = Some(kv);
        }
    }
    Ok(Dynamic::Map(Arc::new(RwLock::new(map))))
}

const TAG_LEN: [usize; 10] = [0, 1, 2, 2, 4, 4, 4, 8, 8, 8];

use bytemuck::pod_collect_to_vec;

fn to_vec(buf: &[u8], tag: u8) -> Result<Dynamic> {
    match tag {
        1 => Ok(Dynamic::from(bytemuck::cast_slice::<_, i8>(buf))),
        2 => {
            let aligned = pod_collect_to_vec::<_, u16>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        3 => {
            let aligned = pod_collect_to_vec::<_, i16>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        4 => {
            let aligned = pod_collect_to_vec::<_, u32>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        5 => {
            let aligned = pod_collect_to_vec::<_, i32>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        6 => {
            let aligned = pod_collect_to_vec::<_, f32>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        7 => {
            let aligned = pod_collect_to_vec::<_, u64>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        8 => {
            let aligned = pod_collect_to_vec::<_, i64>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        9 => {
            let aligned = pod_collect_to_vec::<_, f64>(buf);
            Ok(Dynamic::from(aligned.as_slice()))
        }
        _ => Err(anyhow!("unknow tag {}", tag)),
    }
}

use parking_lot::RwLock;
use std::sync::Arc;

impl MsgUnpack for Dynamic {
    fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        assert_err!(buf.len() < 1, anyhow!("no data"));
        let first_byte = buf[0];
        assert_ok!(first_byte <= 0x7f, (Dynamic::from(first_byte as i64), 1));
        assert_ok!(first_byte >= 0xe0, (Dynamic::from(first_byte as i64 - 256), 1));
        if first_byte >= 0x80 && first_byte <= 0x8f {
            let len = (first_byte & 0x0f) as usize;
            let (value, size) = Self::decode_array(&buf[1..], len * 2)?;
            return vec_to_map(value).map(|r| (r, 1 + size));
        }
        if first_byte >= 0x90 && first_byte <= 0x9f {
            let len = (first_byte & 0x0f) as usize;
            let (value, size) = Self::decode_array(&buf[1..], len)?;
            return Ok((Dynamic::List(Arc::new(RwLock::new(value))), 1 + size));
        }

        if first_byte >= 0xa0 && first_byte <= 0xbf {
            let len = (first_byte & 0x1f) as usize;
            assert_err!(buf.len() < 1 + len, anyhow!("no data"));
            return Ok((Dynamic::from_utf8(&buf[1..1 + len])?, 1 + len));
        }

        assert_ok!(first_byte == 0xc0, (Dynamic::Null, 1));
        assert_err!(first_byte == 0xc1, anyhow!("0xc1 never used"));
        assert_ok!(first_byte == 0xc2, (false.into(), 1));
        assert_ok!(first_byte == 0xc3, (true.into(), 1));

        if first_byte == 0xc4 {
            assert_err!(buf.len() < 2, anyhow!("no data"));
            let len = read_8(&buf[1..]) as usize;
            assert_err!(buf.len() < 2 + len, anyhow!("no data"));
            return Ok((Dynamic::from(&buf[2..2 + len]), 2 + len));
        }

        if first_byte == 0xc5 {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let len = read_16(&buf[1..]) as usize;
            assert_err!(buf.len() < 2 + len, anyhow!("no data"));
            return Ok((Dynamic::from(&buf[3..3 + len]), 3 + len));
        }

        if first_byte == 0xc6 {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let len = read_32(&buf[1..]) as usize;
            assert_err!(buf.len() < 5 + len, anyhow!("no data"));
            return Ok((Dynamic::from(&buf[5..5 + len]), 5 + len));
        }

        if first_byte == 0xc7 {
            assert_err!(buf.len() < 7, anyhow!("no data"));
            let len = read_8(&buf[1..]) as usize;
            let tag = read_8(&buf[2..]);
            let byte_len = len * TAG_LEN[tag as usize];
            return Ok((to_vec(&buf[3..3 + byte_len], tag)?, 3 + byte_len));
        }

        if first_byte == 0xc8 {
            assert_err!(buf.len() < 8, anyhow!("no data"));
            let len = read_16(&buf[1..]) as usize;
            let tag = read_8(&buf[3..]);
            let byte_len = len * TAG_LEN[tag as usize];
            return Ok((to_vec(&buf[4..4 + byte_len], tag)?, 4 + byte_len));
        }

        if first_byte == 0xc9 {
            assert_err!(buf.len() < 10, anyhow!("no data"));
            let len = read_32(&buf[1..]) as usize;
            let tag = read_8(&buf[5..]);
            let byte_len = len * TAG_LEN[tag as usize];
            return Ok((to_vec(&buf[6..6 + byte_len], tag)?, 6 + byte_len));
        }

        if first_byte == 0xca {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let raw_value = read_32(&buf[1..]) as u32;
            let value = f32::from_bits(raw_value);
            return Ok((Dynamic::from(value as f64), 5));
        }

        if first_byte == 0xcb {
            assert_err!(buf.len() < 9, anyhow!("no data"));
            let raw_value = read_64(&buf[1..]);
            let value = f64::from_bits(raw_value);
            return Ok((Dynamic::from(value), 9));
        }

        if first_byte == 0xd0 {
            assert_err!(buf.len() < 2, anyhow!("no data"));
            let raw_value = read_8(&buf[1..]);
            let value = raw_value as i8 as i64;
            return Ok((Dynamic::from(value), 2));
        }

        if first_byte == 0xcd {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let value = read_16(&buf[1..]);
            return Ok((Dynamic::from(value as i64), 3));
        }

        if first_byte == 0xce {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let value = read_32(&buf[1..]);
            return Ok((Dynamic::from(value as i64), 5));
        }

        if first_byte == 0xcf {
            assert_err!(buf.len() < 9, anyhow!("no data"));
            let value = read_64(&buf[1..]);
            return Ok((Dynamic::from(value as i64), 9));
        }

        if first_byte == 0xd3 {
            assert_err!(buf.len() < 9, anyhow!("no data"));
            let raw_value = read_64(&buf[1..]);
            let value = raw_value as i64;
            return Ok((Dynamic::from(value), 9));
        }

        if first_byte == 0xd1 {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let raw_value = read_16(&buf[1..]);
            let value = u16::cast_signed(raw_value) as i64;
            return Ok((Dynamic::from(value), 3));
        }

        if first_byte == 0xd2 {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let raw_value = read_32(&buf[1..]);
            let value = u32::cast_signed(raw_value) as i64;
            return Ok((Dynamic::from(value), 5));
        }

        if first_byte == 0xd3 {
            assert_err!(buf.len() < 9, anyhow!("no data"));
            let raw_value = read_64(&buf[1..]);
            let value = u64::cast_signed(raw_value);
            return Ok((Dynamic::from(value), 9));
        }

        if first_byte == 0xd4 {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let _type_id = u8::cast_signed(buf[1]);
            //let _value = raw[2..3].to_vec();
            return Ok((Dynamic::Null, 3));
        }

        if first_byte == 0xd5 {
            assert_err!(buf.len() < 4, anyhow!("no data"));
            let _type_id = u8::cast_signed(buf[1]);
            return Ok((Dynamic::Null, 4));
        }

        if first_byte == 0xd6 {
            assert_err!(buf.len() < 6, anyhow!("no data"));
            let _type_id = u8::cast_signed(buf[1]);
            return Ok((Dynamic::Null, 6));
        }

        if first_byte == 0xd7 {
            assert_err!(buf.len() < 10, anyhow!("no data"));
            let _type_id = buf[1] as i8;
            return Ok((Dynamic::Null, 10));
        }

        if first_byte == 0xd8 {
            assert_err!(buf.len() < 18, anyhow!("no data"));
            let _type_id = buf[1] as i8;
            return Ok((Dynamic::Null, 18));
        }

        if first_byte == 0xd9 {
            assert_err!(buf.len() < 2, anyhow!("no data"));
            let len = read_8(&buf[1..]) as usize;
            assert_err!(buf.len() < 2 + len, anyhow!("no data"));
            return Ok((Dynamic::from_utf8(&buf[2..2 + len])?, 2 + len));
        }

        if first_byte == 0xda {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let len = read_16(&buf[1..]) as usize;
            assert_err!(buf.len() < 3 + len, anyhow!("no data"));
            return Ok((Dynamic::from_utf8(&buf[3..3 + len])?, 3 + len));
        }

        if first_byte == 0xdb {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let len = read_32(&buf[1..]) as usize;
            assert_err!(buf.len() < 5 + len, anyhow!("no data"));
            return Ok((Dynamic::from_utf8(&buf[5..5 + len])?, 5 + len));
        }

        if first_byte == 0xdc {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let len = read_16(&buf[1..]) as usize;
            let (value, size) = Self::decode_array(&buf[3..], len)?;
            return Ok((Dynamic::List(Arc::new(RwLock::new(value))), 3 + size));
        }

        if first_byte == 0xdd {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let len = read_32(&buf[1..]) as usize;
            let (value, size) = Self::decode_array(&buf[5..], len)?;
            return Ok((Dynamic::List(Arc::new(RwLock::new(value))), 5 + size));
        }

        if first_byte == 0xde {
            assert_err!(buf.len() < 3, anyhow!("no data"));
            let len = read_16(&buf[1..]) as usize;
            let (value, size) = Self::decode_array(&buf[3..], len * 2)?;
            return vec_to_map(value).map(|r| (r, 3 + size));
        }

        if first_byte == 0xdf {
            assert_err!(buf.len() < 5, anyhow!("no data"));
            let len = read_32(&buf[1..]) as usize;
            let (value, size) = Self::decode_array(&buf[5..], len * 2)?;
            return vec_to_map(value).map(|r| (r, 5 + size));
        }
        Err(anyhow!("error code {}", first_byte))
    }
}
