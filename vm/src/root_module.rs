//支持 root 内存和 redis 文件系统
use dynamic::{Dynamic, Type};

use crate::JITRunTime;
use crate::RwLock;
use crate::ZustCallback;
use crate::memory::alloc_dynamic;
use root::{Object, get_mount};
use std::sync::Weak;
extern "C" fn root_add(name: *const Dynamic, value: *const Dynamic) -> bool {
    unsafe {
        let obj = Object::Value((*value).clone());
        root::add(&(*name).as_str(), obj).unwrap_or(false)
    }
}

extern "C" fn root_contains(name: *const Dynamic) -> bool {
    unsafe { if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.contains(name) } else { false } }
}

extern "C" fn root_remove(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(root::remove((*name).as_str()).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_dir(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(root::dir((*name).as_str()).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_keys(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(root::keys((*name).as_str()).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_send(name: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    unsafe {
        let ret = root::send_msg(&(*name).as_str(), (*value).clone()).unwrap_or(Dynamic::Null);
        alloc_dynamic(ret)
    }
}

extern "C" fn root_send_idx(name: *const Dynamic, idx: i64, value: *const Dynamic) -> *const Dynamic {
    unsafe {
        let ret = root::send_idx_msg(&(*name).as_str(), idx as usize, (*value).clone()).unwrap_or(Dynamic::Null);
        alloc_dynamic(ret)
    }
}

extern "C" fn root_add_map(name: *const Dynamic) -> bool {
    unsafe { root::add_map(&(*name).as_str()).is_ok() }
}
extern "C" fn root_add_list(name: *const Dynamic) -> bool {
    unsafe { root::add_list(&(*name).as_str()).is_ok() }
}

extern "C" fn root_mount(name: *const Dynamic, url: *const Dynamic) {
    //以后根据 url 自动选择
    unsafe {
        let _ = root::mount_redis(&(*name).as_str(), &(*url).as_str());
    }
}

extern "C" fn root_mount_fjall(name: *const Dynamic, data_dir: *const Dynamic) {
    unsafe {
        let _ = root::mount_fjall(&(*name).as_str(), &(*data_dir).as_str());
    }
}

extern "C" fn root_mount_dir(name: *const Dynamic, host_dir: *const Dynamic) -> bool {
    unsafe {
        // mount_dir 会校验 host_dir 存在并 canonicalize;失败时记日志但不 panic,
        // 返回 false 让脚本拿到失败信号。
        match root::mount_dir(&(*name).as_str(), &(*host_dir).as_str()) {
            Ok(added) => added,
            Err(e) => {
                log::error!("root::mount_dir 失败: {:#}", e);
                false
            }
        }
    }
}

extern "C" fn root_get(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get(name, |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null }) }
}

extern "C" fn root_len(name: *const Dynamic) -> i64 {
    unsafe { if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.len(name).map(|l| l as i64).unwrap_or(-1) } else { -1 } }
}

extern "C" fn root_push(name: *const Dynamic, value: *const Dynamic) -> i64 {
    unsafe { root::push(&(*name).as_str(), (*value).clone()).map(|idx| idx as i64).unwrap_or(-1) }
}

extern "C" fn root_get_idx(name: *const Dynamic, idx: i64) -> *const Dynamic {
    unsafe { alloc_dynamic(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get_idx(name, idx as usize, |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null }) }
}

extern "C" fn root_remove_idx(name: *const Dynamic, idx: i64) -> *const Dynamic {
    unsafe { alloc_dynamic(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.remove_idx(name, idx as usize).map(|obj| obj.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null }) }
}

extern "C" fn root_insert(name: *const Dynamic, key: *const Dynamic, value: *const Dynamic) {
    unsafe {
        if let Err(err) = root::insert(&(*name).as_str(), &(*key).as_str(), (*value).clone()) {
            log::error!("root::insert failed: {err}");
        }
    }
}

extern "C" fn root_get_key(name: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.get_key(name, &(*key).as_str(), |v| v.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null }) }
}

pub(crate) extern "C" fn root_add_fn_with_vm(context: *const Weak<RwLock<JITRunTime>>, name: *const Dynamic, fn_name: *const Dynamic) -> bool {
    let name = unsafe { (*name).clone() };
    let fn_name = unsafe { (*fn_name).clone() };
    match crate::with_native_context(context, |vm| vm.jit.write().get_fn_ptr(fn_name.as_str(), &[Type::Any])) {
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
    unsafe { alloc_dynamic(if let Ok((m, name)) = get_mount(&(*name).as_str()) { m.remove_key(name, &(*key).as_str()).map(|obj| obj.value()).unwrap_or(Dynamic::Null) } else { Dynamic::Null }) }
}

extern "C" fn root_update(name: *const Dynamic, callback: *const Dynamic) -> *const Dynamic {
    let name = unsafe { (*name).as_str().to_string() };
    let Some(callback) = (unsafe { (&*callback).as_custom::<ZustCallback>() }) else {
        log::error!("root::update {}: 第二个参数不是闭包", name);
        return alloc_dynamic(Dynamic::Null);
    };
    let callback = callback.clone();
    let result = root::update(&name, move |current| callback.call1(current).unwrap_or(Dynamic::Null)).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

extern "C" fn root_update_key(name: *const Dynamic, key: *const Dynamic, callback: *const Dynamic) -> *const Dynamic {
    let name = unsafe { (*name).as_str().to_string() };
    let key = unsafe { (*key).as_str().to_string() };
    let Some(callback) = (unsafe { (&*callback).as_custom::<ZustCallback>() }) else {
        log::error!("root::update_key {}/{}: 第三个参数不是闭包", name, key);
        return alloc_dynamic(Dynamic::Null);
    };
    let callback = callback.clone();
    let result = root::update_key(&name, &key, move |current| callback.call1(current).unwrap_or(Dynamic::Null)).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

pub const ROOT_NATIVE: &[(&str, &[Type], Type, *const u8)] = &[
    ("mount", &[Type::Any, Type::Any], Type::Void, root_mount as *const u8),
    ("mount_fjall", &[Type::Any, Type::Any], Type::Void, root_mount_fjall as *const u8),
    ("mount_dir", &[Type::Any, Type::Any], Type::Bool, root_mount_dir as *const u8),
    ("add_list", &[Type::Any], Type::Bool, root_add_list as *const u8),
    ("add_map", &[Type::Any], Type::Bool, root_add_map as *const u8),
    ("add", &[Type::Any, Type::Any], Type::Bool, root_add as *const u8),
    ("dir", &[Type::Any], Type::Any, root_dir as *const u8),
    ("keys", &[Type::Any], Type::Any, root_keys as *const u8),
    ("remove", &[Type::Any], Type::Any, root_remove as *const u8),
    ("contains", &[Type::Any], Type::Bool, root_contains as *const u8),
    ("send", &[Type::Any, Type::Any], Type::Any, root_send as *const u8),
    ("send_idx", &[Type::Any, Type::I64, Type::Any], Type::Any, root_send_idx as *const u8),
    ("get", &[Type::Any], Type::Any, root_get as *const u8),
    ("len", &[Type::Any], Type::I64, root_len as *const u8),
    ("push", &[Type::Any, Type::Any], Type::I64, root_push as *const u8),
    ("get_idx", &[Type::Any, Type::I64], Type::Any, root_get_idx as *const u8),
    ("remove_idx", &[Type::Any, Type::I64], Type::Any, root_remove_idx as *const u8),
    ("insert", &[Type::Any, Type::Any, Type::Any], Type::Void, root_insert as *const u8),
    ("get_key", &[Type::Any, Type::Any], Type::Any, root_get_key as *const u8),
    ("remove_key", &[Type::Any, Type::Any], Type::Any, root_remove_key as *const u8),
    ("update", &[Type::Any, Type::Any], Type::Any, root_update as *const u8),
    ("update_key", &[Type::Any, Type::Any, Type::Any], Type::Any, root_update_key as *const u8),
];
