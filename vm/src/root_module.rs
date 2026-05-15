//支持 root 内存和 redis 文件系统
use dynamic::{Dynamic, Type};

use root::{Object, get_mount};
extern "C" fn root_add(name: *const Dynamic, value: *const Dynamic) -> bool {
    unsafe {
        let obj = Object::Value(std::ptr::read(value));
        root::add(&(*name).as_str(), obj).unwrap_or(false)
    }
}

extern "C" fn root_contains(name: *const Dynamic) -> bool {
    unsafe { if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.contains(name) } else { false } }
}

extern "C" fn root_remove(name: *const Dynamic) -> *const Dynamic {
    unsafe { Box::into_raw(Box::new(root::remove((*name).as_str()).unwrap_or(Dynamic::Null))) }
}

extern "C" fn root_dir(name: *const Dynamic) -> *const Dynamic {
    unsafe { Box::into_raw(Box::new(root::dir((*name).as_str()).unwrap_or(Dynamic::Null))) }
}

extern "C" fn root_send(name: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    unsafe {
        let ret = root::send_msg(&(*name).as_str(), std::ptr::read(value)).unwrap_or(Dynamic::Null);
        Box::into_raw(Box::new(ret))
    }
}

extern "C" fn root_send_idx(name: *const Dynamic, idx: i64, value: *const Dynamic) {
    unsafe {
        let _ = root::send_idx_msg(&(*name).as_str(), idx as usize, std::ptr::read(value));
    }
}

extern "C" fn root_add_map(name: *const Dynamic) {
    unsafe {
        let _ = root::add_map(&(*name).as_str());
    }
}
extern "C" fn root_add_list(name: *const Dynamic) {
    unsafe {
        let _ = root::add_list(&(*name).as_str());
    }
}

extern "C" fn root_mount(name: *const Dynamic, url: *const Dynamic) {
    //以后根据 url 自动选择
    unsafe {
        let _ = root::mount_redis(&(*name).as_str(), &(*url).as_str());
    }
}

extern "C" fn root_get(name: *const Dynamic) -> *const Dynamic {
    unsafe {
        let v = Box::new(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get(name, |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null });
        Box::into_raw(v)
    }
}

extern "C" fn root_len(name: *const Dynamic) -> i64 {
    unsafe { if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.len(name).map(|l| l as i64).unwrap_or(-1) } else { -1 } }
}

extern "C" fn root_push(name: *const Dynamic, value: *const Dynamic) -> i64 {
    unsafe { root::push(&(*name).as_str(), std::ptr::read(value)).map(|idx| idx as i64).unwrap_or(-1) }
}

extern "C" fn root_get_idx(name: *const Dynamic, idx: i64) -> *const Dynamic {
    unsafe {
        let v = Box::new(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get_idx(name, idx as usize, |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null });
        Box::into_raw(v)
    }
}

extern "C" fn root_remove_idx(name: *const Dynamic, idx: i64) -> *const Dynamic {
    unsafe {
        let v = Box::new(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.remove_idx(name, idx as usize).map(|obj| obj.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null });
        Box::into_raw(v)
    }
}

extern "C" fn root_insert(name: *const Dynamic, key: *const Dynamic, value: *const Dynamic) {
    unsafe {
        let _ = root::insert(&(*name).as_str(), &(*key).as_str(), std::ptr::read(value));
    }
}

extern "C" fn root_get_key(name: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    unsafe {
        let v = Box::new(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get_key(name, &(*key).as_str(), |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null });
        Box::into_raw(v)
    }
}

extern "C" fn root_add_fn(name: *const Dynamic, fn_name: *const Dynamic) -> bool {
    let name = unsafe { (*name).clone() };
    let fn_name = unsafe { (*fn_name).clone() };
    match crate::get_fn(fn_name.as_str(), &[Type::Any]) {
        Ok((fn_ptr, ty)) => {
            if let Ok((m, name)) = get_mount(name.as_str()) {
                return m.add(name, Object::Func(fn_ptr as i64, ty.clone()));
            }
            log::error!("root_add_fn: mount not found for {}", name.as_str());
        }
        Err(e) => {
            log::error!("root_add_fn: get_fn failed for {}: {:?}", fn_name.as_str(), e);
        }
    }
    false
}

extern "C" fn root_remove_key(name: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    unsafe {
        let v = Box::new(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.remove_key(name, &(*key).as_str()).map(|obj| obj.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null });
        Box::into_raw(v)
    }
}

pub const ROOT_NATIVE: [(&str, &[Type], Type, *const u8); 18] = [
    ("mount", &[Type::Any, Type::Any], Type::Void, root_mount as *const u8),
    ("add_list", &[Type::Any], Type::Void, root_add_list as *const u8),
    ("add_map", &[Type::Any], Type::Void, root_add_map as *const u8),
    ("add", &[Type::Any, Type::Any], Type::I32, root_add as *const u8),
    ("dir", &[Type::Any], Type::Any, root_dir as *const u8),
    ("remove", &[Type::Any], Type::Any, root_remove as *const u8),
    ("contains", &[Type::Any], Type::I32, root_contains as *const u8),
    ("send", &[Type::Any, Type::Any], Type::Any, root_send as *const u8),
    ("send_idx", &[Type::Any, Type::I64, Type::Any], Type::Void, root_send_idx as *const u8),
    ("get", &[Type::Any], Type::Any, root_get as *const u8),
    ("len", &[Type::Any], Type::I64, root_len as *const u8),
    ("push", &[Type::Any, Type::Any], Type::I64, root_push as *const u8),
    ("get_idx", &[Type::Any, Type::I64], Type::Any, root_get_idx as *const u8),
    ("remove_idx", &[Type::Any, Type::I64], Type::Any, root_remove_idx as *const u8),
    ("insert", &[Type::Any, Type::Any, Type::Any], Type::Void, root_insert as *const u8),
    ("get_key", &[Type::Any, Type::Any], Type::Any, root_get_key as *const u8),
    ("remove_key", &[Type::Any, Type::Any], Type::Any, root_remove_key as *const u8),
    ("add_fn", &[Type::Any, Type::Any], Type::Bool, root_add_fn as *const u8),
];
