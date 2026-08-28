//支持 root 内存和 redis 文件系统
use dynamic::{Dynamic, Type};

use crate::JITRunTime;
use crate::RwLock;
use crate::ZustCallback;
use crate::memory::alloc_dynamic;
use root::{Mount, Object, get_mount};
use std::sync::Weak;
extern "C" fn root_add(name: *const Dynamic, value: *const Dynamic) -> bool {
    unsafe {
        let obj = Object::Value((*value).clone());
        root::add(&(*name).as_str(), obj).unwrap_or(false)
    }
}

extern "C" fn root_contains(name: *const Dynamic) -> bool {
    unsafe { root::contains(&(*name).as_str()) }
}

extern "C" fn root_remove(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(root::remove((*name).as_str()).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_dir(name: *const Dynamic, all: bool) -> *const Dynamic {
    unsafe { alloc_dynamic(root::dir((*name).as_str(), all).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_stat(name: *const Dynamic) -> *const Dynamic {
    unsafe { alloc_dynamic(root::stat((*name).as_str()).unwrap_or(Dynamic::Null)) }
}

extern "C" fn root_read_text(name: *const Dynamic) -> *const Dynamic {
    unsafe {
        let path = (*name).as_str();
        let result = match root::read_text(path) {
            Ok(content) => dynamic::map!("ok" => true, "path" => path, "content" => content),
            Err(error) => dynamic::map!("ok" => false, "path" => path, "error" => error.to_string()),
        };
        alloc_dynamic(result)
    }
}

extern "C" fn root_read_texts(paths: *const Dynamic) -> *const Dynamic {
    unsafe {
        let Some(paths) = text_paths(&*paths) else {
            return alloc_dynamic(dynamic::map!("ok" => false, "error" => "paths 必须是字符串列表"));
        };
        let result = match root::read_texts(&paths) {
            Ok(read) => {
                let files = read.into_iter().map(|(path, content)| dynamic::map!("ok" => true, "path" => path, "content" => content)).collect::<Vec<_>>();
                let contents = Dynamic::map(Default::default());
                for file in &files {
                    let path = file.get_dynamic("path").unwrap_or(Dynamic::Null).as_str().to_string();
                    contents.insert(&path, file.clone());
                }
                dynamic::map!(
                    "ok" => true,
                    "files" => files.clone(),
                    "results" => files.clone(),
                    "contents" => contents,
                    "paths" => paths
                )
            }
            Err(error) => dynamic::map!("ok" => false, "error" => error.to_string(), "files" => Vec::<Dynamic>::new(), "results" => Vec::<Dynamic>::new()),
        };
        alloc_dynamic(result)
    }
}

extern "C" fn root_write_text(name: *const Dynamic, content: *const Dynamic) -> *const Dynamic {
    unsafe {
        let path = (*name).as_str();
        let result = if !matches!(&*content, Dynamic::String(_) | Dynamic::StringBuf(_)) {
            dynamic::map!("ok" => false, "path" => path, "error" => "content 必须是字符串")
        } else {
            match root::write_text(path, (*content).as_str()) {
                Ok(_) => dynamic::map!("ok" => true, "path" => path),
                Err(error) => dynamic::map!("ok" => false, "path" => path, "error" => error.to_string()),
            }
        };
        alloc_dynamic(result)
    }
}

extern "C" fn root_write_texts(writes: *const Dynamic) -> *const Dynamic {
    unsafe {
        let writes = match text_writes(&*writes) {
            Ok(writes) => writes,
            Err(error) => return alloc_dynamic(dynamic::map!("ok" => false, "error" => error)),
        };
        let result = match root::write_texts(&writes) {
            Ok(paths) => dynamic::map!("ok" => true, "changed" => paths.len() as i64, "paths" => paths),
            Err(error) => dynamic::map!("ok" => false, "error" => error.to_string()),
        };
        alloc_dynamic(result)
    }
}

extern "C" fn root_list_files(name: *const Dynamic, all: bool) -> *const Dynamic {
    unsafe {
        let path = (*name).as_str();
        let result = match root::dir(path, all) {
            Ok(relative_paths) if relative_paths.is_list() => {
                let prefix = path.trim_end_matches('/');
                let paths = (0..relative_paths.len()).filter_map(|index| relative_paths.get_idx(index)).map(|relative| format!("{prefix}/{}", relative.as_str())).collect::<Vec<_>>();
                dynamic::map!("ok" => true, "path" => path, "paths" => paths.clone(), "files" => paths, "relative_paths" => relative_paths)
            }
            Ok(_) => dynamic::map!("ok" => false, "path" => path, "error" => "目录列表结果不是 list"),
            Err(error) => dynamic::map!("ok" => false, "path" => path, "error" => error.to_string()),
        };
        alloc_dynamic(result)
    }
}

fn text_paths(value: &Dynamic) -> Option<Vec<String>> {
    match value {
        Dynamic::List(list) => list
            .read()
            .iter()
            .map(|value| match value {
                Dynamic::String(text) => Some(text.to_string()),
                Dynamic::StringBuf(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
        Dynamic::Map(_) => value.get_dynamic("paths").or_else(|| value.get_dynamic("files")).as_ref().and_then(text_paths),
        _ => None,
    }
}

fn text_writes(value: &Dynamic) -> Result<Vec<(String, String)>, String> {
    if let Dynamic::Map(map) = value {
        if let Some(inner) = value.get_dynamic("writes").or_else(|| value.get_dynamic("files")) {
            return text_writes(&inner);
        }
        if let Some(inner) = value.get_dynamic("contents") {
            return text_writes(&inner);
        }
        let map = map.read();
        let mut writes = Vec::with_capacity(map.len());
        for (path, content) in map.iter() {
            let content = match content {
                Dynamic::String(text) => text.to_string(),
                Dynamic::StringBuf(text) => text.clone(),
                Dynamic::Map(_) => content
                    .get_dynamic("content")
                    .filter(|value| matches!(value, Dynamic::String(_) | Dynamic::StringBuf(_)))
                    .map(|value| value.as_str().to_string())
                    .ok_or_else(|| format!("{} 的 content 必须是字符串", path))?,
                _ => return Err(format!("{} 的 content 必须是字符串", path)),
            };
            writes.push((path.to_string(), content));
        }
        return Ok(writes);
    }
    let Dynamic::List(list) = value else {
        return Err("writes 必须是 [{path, content}] 或 {path: content}".to_string());
    };
    let mut writes = Vec::with_capacity(list.read().len());
    for item in list.read().iter() {
        let path = item
            .get_dynamic("path")
            .filter(|value| matches!(value, Dynamic::String(_) | Dynamic::StringBuf(_)))
            .map(|value| value.as_str().to_string())
            .ok_or_else(|| "每个 write 都必须有字符串 path".to_string())?;
        let content = item
            .get_dynamic("content")
            .filter(|value| matches!(value, Dynamic::String(_) | Dynamic::StringBuf(_)))
            .map(|value| value.as_str().to_string())
            .ok_or_else(|| format!("{} 的 content 必须是字符串", path))?;
        writes.push((path, content));
    }
    Ok(writes)
}

extern "C" fn root_search_text(name: *const Dynamic, needle: *const Dynamic, recursive: bool, max_results: i64) -> *const Dynamic {
    unsafe {
        let path = (*name).as_str();
        let result = match root::search_text(path, (*needle).as_str(), recursive, max_results) {
            Ok(found) => dynamic::map!(
                "ok" => true,
                "path" => path,
                "matches" => found.get_dynamic("matches").unwrap_or_else(|| Vec::<Dynamic>::new().into()),
                "truncated" => found.get_dynamic("truncated").unwrap_or(Dynamic::Bool(false))
            ),
            Err(error) => dynamic::map!("ok" => false, "path" => path, "error" => error.to_string()),
        };
        alloc_dynamic(result)
    }
}

extern "C" fn root_copy_file(source: *const Dynamic, target: *const Dynamic) -> bool {
    unsafe { root::copy_file(&(*source).as_str(), &(*target).as_str()).unwrap_or(false) }
}

extern "C" fn root_rename(source: *const Dynamic, target: *const Dynamic) -> bool {
    unsafe { root::rename(&(*source).as_str(), &(*target).as_str()).unwrap_or(false) }
}

extern "C" fn root_make_dir(name: *const Dynamic) -> bool {
    unsafe { root::make_dir(&(*name).as_str()).unwrap_or(false) }
}

extern "C" fn root_create_dir(name: *const Dynamic) -> bool {
    unsafe { root::create_dir(&(*name).as_str()).unwrap_or(false) }
}

extern "C" fn root_remove_dir(name: *const Dynamic, recursive: bool) -> bool {
    unsafe { root::remove_dir(&(*name).as_str(), recursive).unwrap_or(false) }
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
    // Dir 后端:走 `get_for_object` 特化(从文件 decode,wrap 成 Object::Value);
    // 其它后端走通用 `m.get`。`m.get` 本身对 Dir 永远 Err(那是底层约束,
    // 因为泛型 T 不一定能量成 Object::Value),所以必须在 C 导出层分派。
    unsafe {
        let result = (|| -> Option<Dynamic> {
            let (m, name) = get_mount(&(*name).as_str()).ok()?;
            if matches!(m, Mount::Dir { .. }) { m.get_for_object(name, |v| v.value().clone()).ok() } else { m.get(name, |v| v.value()).ok() }
        })();
        alloc_dynamic(result.unwrap_or(Dynamic::Null))
    }
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

// List 节点的批量读取原语：add_list/push 写入的节点对 root::get 不可见
// （get 只认 Node::Object），get_list 是它的对称读。缺失节点返回 Null，
// 与 get 的缺失语义一致。
extern "C" fn root_get_list(name: *const Dynamic) -> *const Dynamic {
    unsafe {
        alloc_dynamic(root::get_list(&(*name).as_str()).map(Dynamic::list).unwrap_or(Dynamic::Null))
    }
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
    ("dir", &[Type::Any, Type::Bool], Type::Any, root_dir as *const u8),
    ("list_files", &[Type::Any, Type::Bool], Type::Any, root_list_files as *const u8),
    ("stat", &[Type::Any], Type::Any, root_stat as *const u8),
    ("read_text", &[Type::Any], Type::Any, root_read_text as *const u8),
    ("read_texts", &[Type::Any], Type::Any, root_read_texts as *const u8),
    ("read_many", &[Type::Any], Type::Any, root_read_texts as *const u8),
    ("read_files", &[Type::Any], Type::Any, root_read_texts as *const u8),
    ("read_text_files", &[Type::Any], Type::Any, root_read_texts as *const u8),
    ("write_text", &[Type::Any, Type::Any], Type::Any, root_write_text as *const u8),
    ("write_texts", &[Type::Any], Type::Any, root_write_texts as *const u8),
    ("write_many", &[Type::Any], Type::Any, root_write_texts as *const u8),
    ("write_files", &[Type::Any], Type::Any, root_write_texts as *const u8),
    ("write_text_files", &[Type::Any], Type::Any, root_write_texts as *const u8),
    ("write_files_atomic", &[Type::Any], Type::Any, root_write_texts as *const u8),
    ("search_text", &[Type::Any, Type::Any, Type::Bool, Type::I64], Type::Any, root_search_text as *const u8),
    ("copy_file", &[Type::Any, Type::Any], Type::Bool, root_copy_file as *const u8),
    ("rename", &[Type::Any, Type::Any], Type::Bool, root_rename as *const u8),
    ("make_dir", &[Type::Any], Type::Bool, root_make_dir as *const u8),
    ("create_dir", &[Type::Any], Type::Bool, root_create_dir as *const u8),
    ("remove_dir", &[Type::Any, Type::Bool], Type::Bool, root_remove_dir as *const u8),
    ("keys", &[Type::Any], Type::Any, root_keys as *const u8),
    ("remove", &[Type::Any], Type::Any, root_remove as *const u8),
    ("contains", &[Type::Any], Type::Bool, root_contains as *const u8),
    ("send", &[Type::Any, Type::Any], Type::Any, root_send as *const u8),
    ("send_idx", &[Type::Any, Type::I64, Type::Any], Type::Any, root_send_idx as *const u8),
    ("get", &[Type::Any], Type::Any, root_get as *const u8),
    ("get_list", &[Type::Any], Type::Any, root_get_list as *const u8),
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
