use dynamic::Dynamic;
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
        self.dynamics.iter().any(|dynamic| std::ptr::addr_eq(*dynamic as *const Dynamic, ptr))
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

pub(crate) extern "C" fn scope_exit_bytes(value: *const u8, size: i64) -> *mut u8 {
    let size = size.max(0) as usize;
    let bytes = if value.is_null() || size == 0 { Vec::new() } else { unsafe { std::slice::from_raw_parts(value, size).to_vec() } };
    VM_MEMORY.with(|memory| memory.borrow_mut().exit_scope());
    let dst = alloc_struct_bytes(size);
    if !bytes.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }
    }
    dst
}

pub(crate) unsafe fn take_dynamic_return(ptr: *const Dynamic) -> Box<Dynamic> {
    if ptr.is_null() {
        return Box::new(Dynamic::Null);
    }
    VM_MEMORY.with(|memory| if memory.borrow().owns_dynamic(ptr) { Box::new(unsafe { (&*ptr).deep_clone() }) } else { unsafe { Box::from_raw(ptr as *mut Dynamic) } })
}
