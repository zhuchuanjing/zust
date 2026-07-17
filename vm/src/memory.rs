use dynamic::{Dynamic, Type};
use std::cell::RefCell;
use std::mem::{MaybeUninit, align_of, size_of};
use std::ptr;

const INITIAL_CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct ScopeMark {
    chunk: usize,
    offset: usize,
    dynamic_len: usize,
}

struct Chunk {
    bytes: Box<[MaybeUninit<u8>]>,
}

impl Chunk {
    fn new(size: usize) -> Self {
        let mut bytes = Vec::with_capacity(size);
        bytes.resize_with(size, MaybeUninit::uninit);
        Self { bytes: bytes.into_boxed_slice() }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr() as *mut u8
    }

    /// 本 chunk 覆盖的地址区间 [base, end)。用于 O(1)/O(chunks) 判断某指针是否
    /// 由 arena 持有(分配不跨 chunk,故起始地址落在区间内即整块都在内)。
    fn contains(&self, addr: usize) -> bool {
        let base = self.bytes.as_ptr() as usize;
        addr >= base && addr < base + self.bytes.len()
    }
}

struct VmMemory {
    chunks: Vec<Chunk>,
    chunk: usize,
    offset: usize,
    dynamics: Vec<*mut Dynamic>,
    scopes: Vec<ScopeMark>,
}

impl VmMemory {
    fn new() -> Self {
        Self { chunks: vec![Chunk::new(INITIAL_CHUNK_SIZE)], chunk: 0, offset: 0, dynamics: Vec::new(), scopes: Vec::new() }
    }

    fn has_scope(&self) -> bool {
        !self.scopes.is_empty()
    }

    fn owns_dynamic(&self, ptr: *const Dynamic) -> bool {
        // arena 持有的 Dynamic 都落在某个 chunk 的内存区间内;堆上 Box 的不在。
        // 按 chunk 地址区间判断是 O(chunks)(chunk 极少),取代原先 O(dynamics) 线性扫描。
        let addr = ptr as usize;
        self.chunks.iter().any(|chunk| chunk.contains(addr))
    }

    fn enter_scope(&mut self) {
        self.scopes.push(ScopeMark { chunk: self.chunk, offset: self.offset, dynamic_len: self.dynamics.len() });
    }

    fn exit_scope(&mut self) {
        let Some(mark) = self.scopes.pop() else {
            return;
        };
        for ptr in self.dynamics.drain(mark.dynamic_len..).rev() {
            unsafe {
                ptr::drop_in_place(ptr);
            }
        }
        self.chunk = mark.chunk;
        self.offset = mark.offset;
    }

    fn align_up(value: usize, align: usize) -> usize {
        debug_assert!(align.is_power_of_two());
        (value + align - 1) & !(align - 1)
    }

    fn alloc_raw(&mut self, size: usize, align: usize) -> *mut u8 {
        let size = size.max(1);
        let align = align.max(1);
        loop {
            let offset = Self::align_up(self.offset, align);
            if offset + size <= self.chunks[self.chunk].len() {
                self.offset = offset + size;
                return unsafe { self.chunks[self.chunk].ptr().add(offset) };
            }

            let needed = size.max(INITIAL_CHUNK_SIZE).next_power_of_two();
            if self.chunk + 1 == self.chunks.len() {
                self.chunks.push(Chunk::new(needed));
            } else if self.chunks[self.chunk + 1].len() < needed {
                self.chunks.push(Chunk::new(needed));
                self.chunk = self.chunks.len() - 1;
                self.offset = 0;
                continue;
            }
            self.chunk += 1;
            self.offset = 0;
        }
    }

    fn alloc_bytes(&mut self, size: usize) -> *mut u8 {
        self.alloc_raw(size, 8)
    }

    fn alloc_dynamic(&mut self, value: Dynamic) -> *mut Dynamic {
        let ptr = self.alloc_raw(size_of::<Dynamic>(), align_of::<Dynamic>()) as *mut Dynamic;
        unsafe {
            ptr::write(ptr, value);
        }
        self.dynamics.push(ptr);
        ptr
    }
}

thread_local! {
    static VM_MEMORY: RefCell<VmMemory> = RefCell::new(VmMemory::new());
}

pub(crate) fn alloc_struct_bytes(size: usize) -> *mut u8 {
    VM_MEMORY.with(|memory| memory.borrow_mut().alloc_bytes(size))
}

pub(crate) fn alloc_dynamic(value: Dynamic) -> *const Dynamic {
    VM_MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        if memory.has_scope() { memory.alloc_dynamic(value) as *const Dynamic } else { Box::into_raw(Box::new(value)) }
    })
}

/// JIT 守卫代码在整数除零 / `INT_MIN/-1` 溢出时调用,记录运行期错误标志。
/// 边界([`crate::call_jit_isolated`])在调用结束后读取它,把错误降级为失败的
/// 调用而不是进程崩溃。
pub(crate) extern "C" fn arith_fault() {
    dynamic::set_fault("整数除零");
}

pub(crate) extern "C" fn scope_enter() {
    VM_MEMORY.with(|memory| memory.borrow_mut().enter_scope());
}

pub(crate) extern "C" fn scope_exit_void() {
    VM_MEMORY.with(|memory| memory.borrow_mut().exit_scope());
}

pub(crate) extern "C" fn scope_exit_dynamic(value: *const Dynamic) -> *const Dynamic {
    if value.is_null() {
        scope_exit_void();
        return alloc_dynamic(Dynamic::Null);
    }

    let promoted = unsafe { (&*value).deep_clone() };
    VM_MEMORY.with(|memory| memory.borrow_mut().exit_scope());
    alloc_dynamic(promoted)
}

fn clone_dynamic_ptr_fields(bytes: &mut [u8], src_base: *const u8, ty: &Type, offset: usize) {
    match ty {
        Type::Bool | Type::I8 | Type::U8 | Type::I16 | Type::U16 | Type::I32 | Type::U32 | Type::I64 | Type::U64 | Type::F16 | Type::F32 | Type::F64 | Type::Void => {}
        Type::Struct { fields, .. } => {
            let (_, offsets) = Type::struct_layout(fields);
            for ((_, field_ty), field_offset) in fields.iter().zip(offsets) {
                clone_dynamic_ptr_fields(bytes, unsafe { src_base.add(field_offset as usize) }, field_ty, offset + field_offset as usize);
            }
        }
        Type::Array(elem_ty, len) | Type::Vec(elem_ty, len) => {
            let width = elem_ty.storage_width() as usize;
            for idx in 0..*len as usize {
                clone_dynamic_ptr_fields(bytes, unsafe { src_base.add(idx * width) }, elem_ty, offset + idx * width);
            }
        }
        _ => {
            if offset + std::mem::size_of::<usize>() > bytes.len() {
                return;
            }
            let ptr = unsafe { std::ptr::read_unaligned(src_base as *const usize) };
            if ptr == 0 {
                return;
            }
            let cloned = unsafe { (&*(ptr as *const Dynamic)).deep_clone() };
            let boxed = Box::into_raw(Box::new(cloned)) as usize;
            bytes[offset..offset + std::mem::size_of::<usize>()].copy_from_slice(&boxed.to_ne_bytes());
        }
    }
}

pub(crate) extern "C" fn scope_exit_bytes(value: *const u8, size: i64, ty: i64) -> *mut u8 {
    let size = size.max(0) as usize;
    let mut bytes = if value.is_null() || size == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(value, size).to_vec() } };
    if !value.is_null() && ty != 0 {
        let ty = unsafe { &*(ty as *const Type) };
        clone_dynamic_ptr_fields(&mut bytes, value, ty, 0);
    }
    VM_MEMORY.with(|memory| memory.borrow_mut().exit_scope());
    let dst = alloc_struct_bytes(size);
    if !bytes.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }
    }
    dst
}

pub unsafe fn take_dynamic_return(ptr: *const Dynamic) -> Box<Dynamic> {
    if ptr.is_null() {
        return Box::new(Dynamic::Null);
    }
    VM_MEMORY.with(|memory| if memory.borrow().owns_dynamic(ptr) { Box::new(unsafe { (&*ptr).deep_clone() }) } else { unsafe { Box::from_raw(ptr as *mut Dynamic) } })
}
