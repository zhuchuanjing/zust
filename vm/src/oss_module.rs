use crate::memory::alloc_dynamic;
use anyhow::{Result, anyhow};
use dynamic::{Dynamic, Type, map};

extern "C" fn oss_upload(object_name: *const Dynamic, bytes: *const Dynamic) -> *const Dynamic {
    if object_name.is_null() || bytes.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let object_name = unsafe { (&*object_name).clone() };
    let bytes = unsafe { (&*bytes).clone() };
    alloc_dynamic(upload_bytes(object_name, bytes))
}

extern "C" fn oss_signed_url(input: *const Dynamic) -> *const Dynamic {
    if input.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let input = unsafe { (&*input).clone() };
    alloc_dynamic(llm::oss::signed_url_request(input))
}

fn upload_bytes(object_name: Dynamic, bytes: Dynamic) -> Dynamic {
    match upload_bytes_result(object_name, bytes) {
        Ok(result) => result,
        Err(err) => map!("ok"=> false, "error"=> err.to_string()),
    }
}

fn upload_bytes_result(object_name: Dynamic, bytes: Dynamic) -> Result<Dynamic> {
    let object_name = object_name.as_str().to_string();
    if object_name.trim().is_empty() {
        return Err(anyhow!("oss::upload object_name missing"));
    }
    let bytes = bytes.as_bytes().ok_or_else(|| anyhow!("oss::upload expects bytes"))?.to_vec();
    let upload_name = object_name.clone();
    let oss_url = root::sync_await!(async move { llm::oss::upload(&upload_name, bytes).await })?;
    let url = llm::oss::get_link(&oss_url)?;
    Ok(map!("ok"=> true, "object_name"=> object_name, "oss_url"=> oss_url, "url"=> url))
}

pub const OSS_NATIVE: [(&str, &[Type], Type, *const u8); 2] = [
    ("upload", &[Type::Any, Type::Any], Type::Any, oss_upload as *const u8),
    ("signed_url", &[Type::Any], Type::Any, oss_signed_url as *const u8),
];
