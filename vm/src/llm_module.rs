use base64::{Engine as _, engine::general_purpose};
use dynamic::{Dynamic, Type};

extern "C" fn llm_complete(openai: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { openai.read() };
    let value = unsafe { value.read() };
    let result = root::sync_await!(llm::complete(openai, value, None)).unwrap_or(Dynamic::Null);
    Box::into_raw(Box::new(result))
}

extern "C" fn llm_audio(openai: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { openai.read() };
    let value = unsafe { value.read() };
    let text = root::sync_await!(llm::audio_recognize(openai, value)).ok().unwrap_or(Dynamic::Null);
    Box::into_raw(Box::new(text))
}

extern "C" fn llm_deep(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { openai.read() };
    let value = unsafe { value.read() };
    let notifier = unsafe { notifier.read() };
    let id = root::start_task(value.clone(), || {
        Box::pin(async move {
            let r = llm::complete(openai, value, Some(notifier.clone())).await?;
            llm::notify(&notifier, r)?;
            Ok(())
        })
    });
    Box::into_raw(Box::new(id.into()))
}

fn image_url(value: &Dynamic) -> Option<Dynamic> {
    value.get_dynamic("url").or_else(|| value.get_dynamic("image_url")).or_else(|| value.get_dynamic("image"))
}

fn image_b64(value: &Dynamic) -> Option<Dynamic> {
    value.get_dynamic("b64_json").or_else(|| value.get_dynamic("base64")).or_else(|| value.get_dynamic("image_base64"))
}

async fn store_generated_image(result: &Dynamic, image_name: &str) -> Option<String> {
    if let Some(b64) = image_b64(result) {
        if let Ok(data) = general_purpose::STANDARD.decode(b64.as_str()) {
            let _ = root::insert("redis/images", image_name, Dynamic::Bytes(data));
            return Some(format!("/images/{}", image_name));
        }
    }
    if let Some(url) = image_url(result) {
        if let Ok(resp) = reqwest::get(url.as_str()).await {
            if let Ok(data) = resp.bytes().await {
                let _ = root::insert("redis/images", image_name, Dynamic::Bytes(data.to_vec()));
                return Some(format!("/images/{}", image_name));
            }
        }
        return Some(url.as_str().to_string());
    }
    None
}

extern "C" fn llm_image(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { openai.read() };
    let value = unsafe { value.read() };
    let notifier = unsafe { notifier.read() };
    let id = root::start_task(value.clone(), || {
        Box::pin(async move {
            let r = llm::image(openai, value, Some(notifier.clone())).await?;
            if let (Some(world_id), Some(image_id)) = (notifier.get_dynamic("world_id"), notifier.get_dynamic("image_id")) {
                // 从外部 URL 下载图片，存入 redis/images 提供长期访问
                let image_name = format!("world-{}-{}", world_id.as_str(), image_id.as_str());
                let local_url = store_generated_image(&r, &image_name).await.unwrap_or_default();
                // 图片元信息存入 redis/worlds/{id}/images/{image_id}
                let image_key = format!("redis/worlds/{}/images/{}", world_id.as_str(), image_id.as_str());
                let image_info = dynamic::map!();
                image_info.insert("id", image_id.clone());
                image_info.insert("kind", "world_intro");
                image_info.insert("status", "ready");
                if !local_url.is_empty() {
                    image_info.insert("url", local_url.clone());
                }
                if let Some(prompt) = notifier.get_dynamic("prompt") {
                    image_info.insert("prompt", prompt.clone());
                }
                let _ = root::add_value(&image_key, image_info.clone());
                // 通知客户端刷新
                let ws_msg = dynamic::map!("type" => "world_image_ready", "world_id" => world_id.clone(), "image_id" => image_id.clone(), "image" => image_info);
                let _ = llm::notify(&notifier, ws_msg);
            } else {
                let _ = llm::notify(&notifier, r);
            }
            Ok(())
        })
    });
    Box::into_raw(Box::new(id.into()))
}

pub const LLM_NATIVE: [(&str, &[Type], Type, *const u8); 4] = [
    ("complete", &[Type::Any, Type::Any], Type::Any, llm_complete as *const u8),
    ("image", &[Type::Any, Type::Any, Type::Any], Type::Any, llm_image as *const u8),
    ("audio", &[Type::Any, Type::Any], Type::Any, llm_audio as *const u8),
    ("deep", &[Type::Any, Type::Any, Type::Any], Type::Any, llm_deep as *const u8),
];
