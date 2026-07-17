use anyhow::{Result, anyhow};
use dynamic::{Dynamic, FromJson, FromYaml, ToJson};
use std::io::Write;

#[cfg(feature = "candle")]
pub mod candle;
pub mod oss;

fn debug_log_event(options: &Dynamic, event: Dynamic) {
    let Some(path) = options.get_dynamic("debug_log_file") else {
        return;
    };
    let path = path.as_str().to_string();
    if path.is_empty() {
        return;
    }
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut line = String::new();
    event.to_json(&mut line);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

pub fn to_markdown(d: &Dynamic, buf: &mut String) {
    if d.is_vec() {
        //简单的 Vec<float> Vec<int> 按照 json
        d.to_json(buf);
    } else if let Dynamic::Map(m) = d {
        let map = m.read();
        for (key, v) in map.iter() {
            buf.push_str(&format!("#### ```{}```\n", key));
            to_markdown(v, buf);
            buf.push('\n');
        }
    } else if let Dynamic::Bytes(bytes) = d {
        if bytes.len() >= 8 {
            buf.push_str(&format!("[{}...]", hex::encode(&bytes[..8])));
        } else {
            buf.push_str(&d.to_string());
        }
    } else {
        let len = d.len();
        if len >= 1 {
            for idx in 0..len {
                if let Some(item) = d.get_idx(idx) {
                    buf.push_str("- ");
                    to_markdown(&item, buf);
                    buf.push('\n');
                } else {
                    buf.push_str(&d.to_string());
                    break;
                }
            }
        } else {
            buf.push_str(&d.to_string());
        }
    }
}

use dynamic::{list, map};
use futures_util::stream::StreamExt;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::BufReader;
use tokio_util::io::StreamReader; // 关键转换工具

type HmacSha256 = Hmac<Sha256>;

// llm 配置必须显式提供 endpoint (url) 和 model 名。
// 既不允许从 model 前缀猜 url，也不允许 url/model 缺失时静默使用默认值。
fn normalize_provider(options: Dynamic) -> Result<Dynamic> {
    let url = options.get_dynamic("url").ok_or_else(|| anyhow!("llm 配置缺少 endpoint (url)；必须显式提供 url"))?;
    if url.as_str().trim().is_empty() {
        return Err(anyhow!("llm 配置 url 为空；必须显式提供 endpoint"));
    }
    Ok(options)
}

fn with_kind_model(options: Dynamic, kind: &str) -> Result<Dynamic> {
    let options = normalize_provider(options)?;
    let kind_hint = match kind {
        "complete" => options.get_dynamic("vision_model").or_else(|| options.get_dynamic("text_model")),
        "image" => options.get_dynamic("image_model"),
        "tts" => options.get_dynamic("tts_model").or_else(|| options.get_dynamic("audio_model")),
        "audio" => options.get_dynamic("asr_model").or_else(|| options.get_dynamic("audio_model")),
        _ => None,
    };
    let model = kind_hint.or_else(|| options.get_dynamic("model")).ok_or_else(|| anyhow!("llm 配置缺少 model 名；必须显式提供 model（或 {kind}_model）"))?;
    if model.as_str().trim().is_empty() {
        return Err(anyhow!("llm 配置 model 为空；必须显式提供 model 名"));
    }
    options.insert("model", model);
    Ok(options)
}

pub async fn complete(bigmodel: Dynamic, msg: Dynamic, tx: Option<Dynamic>) -> Result<Dynamic> {
    let bigmodel = with_kind_model(bigmodel, "complete")?;
    if uses_responses_api(&bigmodel, &msg) {
        let body = if msg.is_map() && msg.contains("input") { msg } else { map!("input"=> list!(map!("role"=> "user", "content"=> response_content(msg)))) };
        return post("responses", bigmodel, body, tx).await;
    }

    if msg.is_map() && msg.contains("messages") {
        post("chat/completions", bigmodel, msg, tx).await
    } else {
        post("chat/completions", bigmodel, map!("messages"=> list!(map!("role"=> "user", "content"=> chat_content(msg)))), tx).await
    }
}

fn uses_responses_api(openai: &Dynamic, msg: &Dynamic) -> bool {
    let configured = openai.get_dynamic("api").or_else(|| openai.get_dynamic("endpoint")).or_else(|| openai.get_dynamic("method")).is_some_and(|api| api.as_str() == "responses");

    configured || contains_responses_content(msg)
}

fn contains_responses_content(msg: &Dynamic) -> bool {
    if msg.is_map() {
        if let Some(ty) = msg.get_dynamic("type") {
            return matches!(ty.as_str(), "input_file" | "input_text" | "input_image");
        }
        if let Some(input) = msg.get_dynamic("input") {
            return contains_responses_content(&input);
        }
        if let Some(content) = msg.get_dynamic("content") {
            return contains_responses_content(&content);
        }
        return false;
    }

    if msg.is_list() {
        for idx in 0..msg.len() {
            if let Some(item) = msg.get_idx(idx)
                && contains_responses_content(&item)
            {
                return true;
            }
        }
    }

    false
}

fn chat_content(msg: Dynamic) -> Dynamic {
    if msg.is_list() {
        let mut content = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            if let Some(item) = msg.get_idx(idx) {
                content.push(chat_content_item(item));
            }
        }
        return content;
    }

    if msg.is_map() {
        if let Some(content) = msg.get_dynamic("content") {
            return chat_content(content);
        }

        let mut content = Dynamic::list(Vec::new());
        if let Some(text) = msg.get_dynamic("text").or_else(|| msg.get_dynamic("prompt")) {
            content.push(map!("type"=> "text", "text"=> text));
        }
        push_media_items(&mut content, &msg, &["image", "image_url"], "image");
        push_media_items(&mut content, &msg, &["images"], "image");
        push_media_items(&mut content, &msg, &["video", "video_url"], "video");
        push_media_items(&mut content, &msg, &["videos"], "video");
        if content.len() > 0 {
            return content;
        }
    }

    let mut msg_text = String::new();
    to_markdown(&msg, &mut msg_text);
    msg_text.into()
}

fn push_media_items(content: &mut Dynamic, msg: &Dynamic, keys: &[&str], media_type: &str) {
    for key in keys {
        if let Some(value) = msg.get_dynamic(key) {
            push_media_value(content, value, media_type);
        }
    }
}

fn push_media_value(content: &mut Dynamic, value: Dynamic, media_type: &str) {
    if value.is_list() {
        for idx in 0..value.len() {
            if let Some(item) = value.get_idx(idx) {
                push_media_value(content, item, media_type);
            }
        }
        return;
    }

    content.push(match media_type {
        "video" => video_content_item(value),
        _ => image_content_item(value),
    });
}

fn image_content_item(item: Dynamic) -> Dynamic {
    if item.is_map() && item.contains("type") {
        item
    } else if item.is_map() {
        if let Some(url) = item.get_dynamic("url").or_else(|| item.get_dynamic("image_url")).or_else(|| item.get_dynamic("image")) {
            map!("type"=> "image_url", "image_url"=> map!("url"=> url))
        } else {
            map!("type"=> "text", "text"=> item)
        }
    } else {
        map!("type"=> "image_url", "image_url"=> map!("url"=> item))
    }
}

fn video_content_item(item: Dynamic) -> Dynamic {
    if item.is_map() && item.contains("type") {
        item
    } else if item.is_map() {
        if let Some(url) = item.get_dynamic("url").or_else(|| item.get_dynamic("video_url")).or_else(|| item.get_dynamic("video")) {
            map!("type"=> "video", "video"=> list!(url))
        } else {
            map!("type"=> "text", "text"=> item)
        }
    } else {
        map!("type"=> "video", "video"=> list!(item))
    }
}

fn chat_content_item(item: Dynamic) -> Dynamic {
    if item.is_map() && item.contains("type") {
        item
    } else if item.is_str() {
        let text = item.as_str();
        if text.starts_with("data:video/") || looks_like_video_url(text) {
            video_content_item(item)
        } else if text.starts_with("data:image/") || looks_like_image_url(text) || text.starts_with("http://") || text.starts_with("https://") {
            image_content_item(item)
        } else {
            map!("type"=> "text", "text"=> item)
        }
    } else {
        map!("type"=> "text", "text"=> item)
    }
}

fn looks_like_image_url(text: &str) -> bool {
    (text.starts_with("http://") || text.starts_with("https://")) && [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp"].iter().any(|suffix| text.to_ascii_lowercase().contains(suffix))
}

fn looks_like_video_url(text: &str) -> bool {
    (text.starts_with("http://") || text.starts_with("https://")) && [".mp4", ".mov", ".webm", ".m4v", ".avi"].iter().any(|suffix| text.to_ascii_lowercase().contains(suffix))
}

fn response_content(msg: Dynamic) -> Dynamic {
    if msg.is_list() {
        let mut list = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            if let Some(item) = msg.get_idx(idx) {
                list.push(response_content_item(item));
            }
        }
        list
    } else {
        Dynamic::list(vec![response_content_item(msg)])
    }
}

fn response_content_item(item: Dynamic) -> Dynamic {
    if item.is_map() && item.contains("type") {
        item
    } else {
        let mut text = String::new();
        to_markdown(&item, &mut text);
        map!("type"=> "input_text", "text"=> text)
    }
}

pub async fn image(openai: Dynamic, msg: Dynamic, tx: Option<Dynamic>) -> Result<Dynamic> {
    let openai = with_kind_model(openai, "image")?;
    let result = if uses_kling_image_expand(&openai) {
        post("v1/images/editing/expand", openai.clone(), kling_image_expand_body(msg), tx).await?
    } else if uses_kling_image_generation(&openai) {
        post("v1/images/generations", openai.clone(), kling_image_generation_body(msg), tx).await?
    } else if uses_dashscope_multimodal_image(&openai) {
        post("services/aigc/multimodal-generation/generation", openai.clone(), dashscope_image_body(msg), tx).await?
    } else {
        post("images/generations", openai.clone(), image_body(msg), tx).await?
    };
    image_url_result(&openai, result).await
}

fn image_body(msg: Dynamic) -> Dynamic {
    if msg.is_map() {
        if msg.contains("prompt") || msg.contains("input") {
            let body = msg.deep_clone();
            if !body.contains("prompt")
                && let Some(input) = body.remove_dynamic("input")
            {
                body.insert("prompt", input);
            }
            return body;
        }
        if let Some(text) = msg.get_dynamic("text") {
            let body = map!("prompt"=> text);
            copy_image_urls(&body, &msg);
            return body;
        }
    }

    if msg.is_list() {
        let mut list = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            if let Some(item) = msg.get_idx(idx) {
                list.push(item);
            }
        }
        map!("prompt"=> list.to_markdown())
    } else {
        let mut msg_text = String::new();
        to_markdown(&msg, &mut msg_text);
        map!("prompt"=> msg_text)
    }
}

fn copy_image_urls(body: &Dynamic, msg: &Dynamic) {
    for key in ["image", "image_url", "images"] {
        if let Some(value) = msg.get_dynamic(key) {
            body.insert(key, value);
        }
    }
}

fn uses_dashscope_multimodal_image(options: &Dynamic) -> bool {
    let kind = options.get_dynamic("kind").map(|v| v.as_str().to_string()).unwrap_or_default();
    if kind == "dashscope_qwen_image_edit" || kind == "dashscope_multimodal_image" {
        return true;
    }

    let url = options.get_dynamic("url").map(|v| v.as_str().to_string()).unwrap_or_default();
    let model = options.get_dynamic("model").map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    url.contains("dashscope.aliyuncs.com/api/v1") && (model.starts_with("qwen-image") || model.starts_with("wan"))
}

fn uses_kling_image_expand(options: &Dynamic) -> bool {
    let kind = options.get_dynamic("kind").map(|v| v.as_str().to_string()).unwrap_or_default();
    if kind == "kling_image_expand" {
        return true;
    }

    let url = options.get_dynamic("url").map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    url.contains("klingai.com") && url.contains("images/editing/expand")
}

fn uses_kling_image_generation(options: &Dynamic) -> bool {
    let kind = options.get_dynamic("kind").map(|v| v.as_str().to_string()).unwrap_or_default();
    if kind == "kling_image_generation" {
        return true;
    }

    let url = options.get_dynamic("url").map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    url.contains("klingai.com") && url.contains("images/generations")
}

fn uses_kling(options: &Dynamic) -> bool {
    let kind = options.get_dynamic("kind").map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    if kind.starts_with("kling") {
        return true;
    }

    let provider = options.get_dynamic("provider").or_else(|| options.get_dynamic("brand")).or_else(|| options.get_dynamic("name")).map(|v| v.as_str().to_ascii_lowercase()).unwrap_or_default();
    if provider.contains("kling") {
        return true;
    }

    options.get_dynamic("url").map(|v| v.as_str().to_ascii_lowercase()).is_some_and(|url| url.contains("klingai.com"))
}

fn auth_value(options: &Dynamic, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| options.get_dynamic(key).map(|v| v.as_str().to_string()).filter(|v| !v.is_empty()))
}

fn bearer_token(options: &Dynamic) -> Result<Option<String>> {
    if let Some(key) = auth_value(options, &["key", "api_key", "apiKey", "token"]) {
        return Ok(Some(key));
    }

    if !uses_kling(options) {
        return Ok(None);
    }

    let Some(access_key) = auth_value(options, &["access_key", "accessKey", "access_id", "accessId", "ak"]) else {
        return Ok(None);
    };
    let secret_key = auth_value(options, &["secret_key", "secretKey", "access_secret", "accessSecret", "sk"]).ok_or_else(|| anyhow!("Kling 配置缺少 secret_key"))?;
    Ok(Some(kling_jwt_token(&access_key, &secret_key)?))
}

fn kling_jwt_token(access_key: &str, secret_key: &str) -> Result<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let header = map!("alg"=> "HS256", "typ"=> "JWT");
    let payload = map!("iss"=> access_key, "exp"=> now + 1800, "nbf"=> now - 5);
    let mut header_json = String::new();
    let mut payload_json = String::new();
    header.to_json(&mut header_json);
    payload.to_json(&mut payload_json);

    let header_part = general_purpose::URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_part = general_purpose::URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let signing_input = format!("{header_part}.{payload_part}");
    let mut mac = HmacSha256::new_from_slice(secret_key.as_bytes())?;
    mac.update(signing_input.as_bytes());
    let signature = general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{signing_input}.{signature}"))
}

fn kling_image_generation_body(msg: Dynamic) -> Dynamic {
    if msg.is_map() {
        let body = msg.deep_clone();
        if !body.contains("prompt")
            && let Some(input) = body.remove_dynamic("input").or_else(|| body.remove_dynamic("text"))
        {
            body.insert("prompt", input);
        }
        if !body.contains("model_name")
            && let Some(model) = body.remove_dynamic("model")
        {
            body.insert("model_name", model);
        }
        if !body.contains("n") {
            body.insert("n", 1);
        }
        return body;
    }

    map!("prompt"=> msg, "n"=> 1)
}

fn kling_image_expand_body(msg: Dynamic) -> Dynamic {
    if msg.is_map() {
        let body = msg.deep_clone();
        if !body.contains("image") {
            if let Some(image) = body.get_dynamic("image_url").or_else(|| body.get_dynamic("url")) {
                body.insert("image", image);
            }
        }
        if !body.contains("prompt")
            && let Some(input) = body.remove_dynamic("input").or_else(|| body.remove_dynamic("text"))
        {
            body.insert("prompt", input);
        }
        if !body.contains("n") {
            body.insert("n", 1);
        }
        if !body.contains("up_expansion_ratio") {
            body.insert("up_expansion_ratio", 0);
        }
        if !body.contains("down_expansion_ratio") {
            body.insert("down_expansion_ratio", 0);
        }
        if !body.contains("left_expansion_ratio") {
            body.insert("left_expansion_ratio", 0);
        }
        if !body.contains("right_expansion_ratio") {
            body.insert("right_expansion_ratio", 0);
        }
        return body;
    }

    map!(
        "prompt"=> msg,
        "n"=> 1,
        "up_expansion_ratio"=> 0,
        "down_expansion_ratio"=> 0,
        "left_expansion_ratio"=> 0,
        "right_expansion_ratio"=> 0
    )
}

fn dashscope_image_body(msg: Dynamic) -> Dynamic {
    let parameters = dashscope_image_parameters(&msg);
    let mut content = Dynamic::list(Vec::new());
    let mut prompt = Dynamic::Null;

    if msg.is_map() {
        if let Some(text) = msg.get_dynamic("prompt").or_else(|| msg.get_dynamic("text")).or_else(|| msg.get_dynamic("input")) {
            prompt = text;
        }
        for key in ["image", "image_url"] {
            if let Some(image) = msg.get_dynamic(key) {
                content.push(map!("image"=> image));
            }
        }
        for key in ["images", "referenceImages", "reference_images"] {
            if let Some(images) = msg.get_dynamic(key) {
                push_dashscope_images(&mut content, images);
            }
        }
    } else {
        prompt = msg;
    }

    if !prompt.is_null() {
        content.push(map!("text"=> prompt));
    }

    let body = map!(
        "input"=> map!(
            "messages"=> list!(
                map!("role"=> "user", "content"=> content)
            )
        )
    );
    body.insert("parameters", parameters);
    body
}

fn push_dashscope_images(content: &mut Dynamic, images: Dynamic) {
    if images.is_list() {
        for idx in 0..images.len() {
            if let Some(image) = images.get_idx(idx) {
                push_dashscope_images(content, image);
            }
        }
    } else if images.is_map() {
        if let Some(url) = images.get_dynamic("url").or_else(|| images.get_dynamic("image")).or_else(|| images.get_dynamic("image_url")) {
            content.push(map!("image"=> url));
        }
    } else if images.is_str() {
        content.push(map!("image"=> images));
    }
}

fn dashscope_image_parameters(msg: &Dynamic) -> Dynamic {
    let parameters = map!();
    for key in ["size", "n", "watermark", "seed", "negative_prompt", "prompt_extend", "image_size", "bbox_list"] {
        if let Some(value) = msg.get_dynamic(key) {
            let value = if (key == "size" || key == "image_size") && value.is_str() { Dynamic::from(value.as_str().replace('x', "*")) } else { value };
            parameters.insert(key, value);
        }
    }
    parameters
}

async fn image_url_result(options: &Dynamic, result: Dynamic) -> Result<Dynamic> {
    if let Some(url) = find_download_url(&result) {
        return Ok(map!("url"=> url));
    }

    if let Some(task_id) = result.get_dynamic("task_id") {
        return poll_image_task(options, task_id.as_str()).await;
    }

    Err(anyhow!("图片模型结果缺少可下载 url"))
}

async fn poll_image_task(options: &Dynamic, task_id: &str) -> Result<Dynamic> {
    let url = options.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let task_url = if uses_kling_image_expand(options) {
        kling_image_expand_task_url(url.as_str(), task_id)
    } else if uses_kling_image_generation(options) {
        kling_image_generation_task_url(url.as_str(), task_id)
    } else {
        dashscope_task_url(url.as_str(), task_id)
    };
    let interval_ms = options.get_dynamic("task_poll_interval_ms").and_then(|v| v.as_int()).unwrap_or(2000).max(200) as u64;
    let max_polls = options.get_dynamic("task_poll_max").and_then(|v| v.as_int()).unwrap_or(180).max(1);
    let client = http_client_builder(options).timeout(http_timeout(options)).build()?;

    for _ in 0..max_polls {
        let mut req = client.get(&task_url);
        if let Some(token) = bearer_token(options)? {
            req = req.header("authorization", format!("Bearer {}", token));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        debug_log_event(
            options,
            map!(
                "event"=> "llm_image_task_poll_response",
                "url"=> task_url.clone(),
                "taskId"=> task_id,
                "status"=> status.as_u16() as i64,
                "body"=> text.clone()
            ),
        );
        if !status.is_success() {
            return Err(anyhow!("图片任务查询失败 HTTP {}: {}", status.as_u16(), text.trim()));
        }

        let (body, _) = Dynamic::from_json(text.as_bytes())?;
        if let Some(url) = find_download_url(&body) {
            return Ok(map!("url"=> url));
        }

        for output in [body.get_dynamic("data"), body.get_dynamic("output"), Some(body)].into_iter().flatten() {
            if let Some(status) = output.get_dynamic("task_status") {
                let status = status.as_str();
                if matches!(status, "FAILED" | "CANCELED" | "UNKNOWN" | "failed" | "canceled" | "unknown") {
                    let message = output.get_dynamic("task_status_msg").or_else(|| output.get_dynamic("message")).map(|v| v.as_str().to_string()).unwrap_or_else(|| status.to_string());
                    return Err(anyhow!("图片任务失败: {}", message));
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
    }

    Err(anyhow!("图片任务超时: {}", task_id))
}

fn http_timeout(options: &Dynamic) -> std::time::Duration {
    let timeout_ms = options.get_dynamic("request_timeout_ms").or_else(|| options.get_dynamic("timeout_ms")).and_then(|v| v.as_int()).unwrap_or(30000).max(1000) as u64;
    std::time::Duration::from_millis(timeout_ms)
}

fn dashscope_task_url(url: &str, task_id: &str) -> String {
    let base = url.trim_end_matches('/');
    let base = if let Some((prefix, _)) = base.split_once("/services/") {
        prefix
    } else if let Some((prefix, _)) = base.split_once("/compatible-mode/") {
        prefix
    } else {
        base
    };
    format!("{}/tasks/{}", base.trim_end_matches('/'), task_id)
}

fn kling_image_expand_task_url(url: &str, task_id: &str) -> String {
    let base = url.trim_end_matches('/');
    if base.ends_with("/v1/images/editing/expand") { format!("{}/{}", base, task_id) } else { format!("{}/v1/images/editing/expand/{}", base, task_id) }
}

fn kling_image_generation_task_url(url: &str, task_id: &str) -> String {
    let base = url.trim_end_matches('/');
    if base.ends_with("/v1/images/generations") { format!("{}/{}", base, task_id) } else { format!("{}/v1/images/generations/{}", base, task_id) }
}

fn find_download_url(value: &Dynamic) -> Option<Dynamic> {
    if value.is_str() {
        let text = value.as_str();
        if is_download_url(text) {
            return Some(value.clone());
        }
        return None;
    }

    if value.is_list() {
        for idx in 0..value.len() {
            if let Some(item) = value.get_idx(idx)
                && let Some(url) = find_download_url(&item)
            {
                return Some(url);
            }
        }
        return None;
    }

    if !value.is_map() {
        return None;
    }

    for key in ["url", "image_url", "output_url", "output_image_url", "image"] {
        if let Some(item) = value.get_dynamic(key)
            && let Some(url) = find_download_url(&item)
        {
            return Some(url);
        }
    }

    for key in ["output", "results", "choices", "message", "content", "data", "task_result", "images"] {
        if let Some(item) = value.get_dynamic(key)
            && let Some(url) = find_download_url(&item)
        {
            return Some(url);
        }
    }

    None
}

fn is_download_url(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://") || text.starts_with("data:image/")
}

pub async fn tts(openai: Dynamic, msg: Dynamic) -> Result<Dynamic> {
    let openai = with_kind_model(openai, "tts")?;
    let body = if msg.is_map() {
        if !msg.contains("input") {
            if let Some(text) = msg.remove_dynamic("text") {
                msg.insert("input", text);
            }
        }
        if msg.contains("input") {
            msg
        } else {
            let mut msg_text = String::new();
            to_markdown(&msg, &mut msg_text);
            map!("input"=> msg_text)
        }
    } else {
        let mut msg_text = String::new();
        to_markdown(&msg, &mut msg_text);
        map!("input"=> msg_text)
    };

    post_binary("audio/speech", openai, body).await
}

fn normalize_function_schema(function: Dynamic) -> Dynamic {
    if function.is_map() {
        if !function.contains("parameters") {
            function.insert("parameters", map!("type"=> "object", "properties"=> map!()));
        }
        function
    } else {
        function
    }
}

fn normalize_glm_tool(tool: Dynamic) -> Dynamic {
    if !tool.is_map() {
        return tool;
    }

    if let Some(function) = tool.get_dynamic("function") {
        return map!(
            "type"=> "function",
            "function"=> normalize_function_schema(function)
        );
    }

    let is_function_tool = tool.get_dynamic("type").map(|ty| ty.as_str() == "function").unwrap_or(false) || tool.contains("name");

    if !is_function_tool {
        return tool;
    }

    let function = map!();
    if let Some(name) = tool.get_dynamic("name") {
        function.insert("name", name);
    }
    if let Some(description) = tool.get_dynamic("description") {
        function.insert("description", description);
    }
    if let Some(parameters) = tool.get_dynamic("parameters") {
        function.insert("parameters", parameters);
    } else {
        function.insert("parameters", map!("type"=> "object", "properties"=> map!()));
    }
    if let Some(strict) = tool.get_dynamic("strict") {
        function.insert("strict", strict);
    }

    map!("type"=> "function", "function"=> function)
}

fn normalize_glm_tools(msg: &Dynamic) {
    if let Some(tools) = msg.get_dynamic("tools") {
        if tools.is_list() {
            let tool_count = tools.len().min(9);
            let mut normalized = Vec::with_capacity(tool_count);
            for idx in 0..tool_count {
                if let Some(tool) = tools.get_idx(idx) {
                    normalized.push(normalize_glm_tool(tool));
                }
            }
            msg.insert("tools", Dynamic::list(normalized));
        }
    }
}

pub async fn embed<T: TryFrom<Dynamic> + 'static>(openai: Dynamic, msg: Dynamic) -> Result<Vec<T>> {
    let mut msg_text = String::new();
    to_markdown(&msg, &mut msg_text);
    post("embeddings", openai, map!("input"=> msg_text), None).await?.remove_dynamic("embedding").and_then(|v| v.into_vec::<T>()).ok_or(anyhow!("没有生成 arrow"))
}

pub fn notify(tx: &Dynamic, msg: Dynamic) -> Result<()> {
    if tx.is_str() {
        root::send_msg(tx.as_str(), msg)?;
        Ok(())
    } else {
        if let Some(key) = tx.get_dynamic("key") {
            if let Some(idx) = tx.get_dynamic("idx").and_then(|idx| idx.as_int()) {
                let msg = tx.clone() + msg;
                root::send_idx_msg(key.as_str(), idx as usize, msg)?;
            } else {
                let msg = tx.clone() + msg;
                root::send_msg(key.as_str(), msg)?;
            }
        }
        Ok(())
    }
}

use base64::{Engine as _, engine::general_purpose};
pub async fn audio_recognize(bigmodel: Dynamic, audio: Dynamic) -> Result<Dynamic> {
    let bigmodel = with_kind_model(bigmodel, "audio")?;
    let url = bigmodel.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let app_id = bigmodel.get_dynamic("app_id").ok_or(anyhow!("没有 app_id"))?;
    let access_token = bigmodel.get_dynamic("access_token").ok_or(anyhow!("没有 access_token"))?;
    let audio = normalize_audio_payload(audio)?;

    let body = map!("user"=> map!("uid"=> app_id.clone()), "audio"=> audio, "request"=> map!("model_name"=> "bigmodel"));

    let client = shared_http_client(&bigmodel)?;
    let mut body_str = String::new();
    body.to_json(&mut body_str);
    let resp = client
        .post(url.as_str())
        .header("X-Api-App-Key", app_id.as_str())
        .header("X-Api-Access-Key", access_token.as_str())
        .header("X-Api-Resource-Id", "volc.bigasr.auc_turbo")
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .header("X-Api-Sequence", -1)
        .body(body_str)
        .send()
        .await?;
    let (t, _) = Dynamic::from_json(resp.text().await?.as_bytes())?;
    t.get_dynamic("result").and_then(|r| r.get_dynamic("text")).ok_or(anyhow!("没有文字结果"))
}

fn normalize_audio_payload(audio: Dynamic) -> Result<Dynamic> {
    if audio.contains("url") {
        let source = audio.get_dynamic("url").unwrap();
        let source_text = source.as_str().trim();
        if source_text.starts_with("data:") { if let Some((_, data)) = source_text.split_once(',') { Ok(map!("data"=> data)) } else { Ok(audio) } } else { Ok(audio) }
    } else if audio.contains("data") {
        let audio_base64 = general_purpose::STANDARD.encode(audio.get_dynamic("data").unwrap().as_bytes().ok_or(anyhow!("没有音频数据"))?);
        Ok(map!("data"=> audio_base64))
    } else if audio.contains("audio") {
        let audio_base64 = general_purpose::STANDARD.encode(audio.get_dynamic("audio").unwrap().as_bytes().ok_or(anyhow!("没有音频数据"))?);
        Ok(map!("data"=> audio_base64))
    } else {
        Err(anyhow!("没有 url 也没有 data"))
    }
}

fn copy_request_options(options: &Dynamic, msg: &Dynamic) {
    copy_request_options_except(options, msg, &[]);
}

fn no_proxy_enabled(options: &Dynamic) -> bool {
    options.get_dynamic("no_proxy").and_then(|value| value.as_bool()).unwrap_or(false)
}

fn duration_option_ms(options: &Dynamic, keys: &[&str]) -> Option<std::time::Duration> {
    keys.iter().find_map(|key| options.get_dynamic(key).and_then(|value| value.as_int())).map(|millis| std::time::Duration::from_millis(millis.max(1000) as u64))
}

// 统一构造所有 zust-llm HTTP client。`no_proxy: true` 必须作用于文本、
// 二进制、显式 stream、音频与异步任务轮询，而不只是 chat post。
fn http_client_builder(options: &Dynamic) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(duration_option_ms(options, &["connect_timeout_ms"]).unwrap_or(std::time::Duration::from_secs(30)))
        // 这是单次 socket read 的空闲超时；SSE 持续产出 chunk 时会不断重置，
        // 不限制完整大文件响应的总生成时间。
        .read_timeout(duration_option_ms(options, &["read_timeout_ms"]).unwrap_or(std::time::Duration::from_secs(120)));
    if let Some(timeout) = duration_option_ms(options, &["request_timeout_ms", "timeout_ms"]) {
        builder = builder.timeout(timeout);
    }
    if no_proxy_enabled(options) { builder.no_proxy() } else { builder }
}

fn http_client_cache_key(options: &Dynamic) -> String {
    let connect = duration_option_ms(options, &["connect_timeout_ms"]).unwrap_or(std::time::Duration::from_secs(30));
    let read = duration_option_ms(options, &["read_timeout_ms"]).unwrap_or(std::time::Duration::from_secs(120));
    let request = duration_option_ms(options, &["request_timeout_ms", "timeout_ms"]);
    format!(
        "no_proxy={};connect_ms={};read_ms={};request_ms={}",
        no_proxy_enabled(options),
        connect.as_millis(),
        read.as_millis(),
        request.map(|value| value.as_millis().to_string()).unwrap_or_else(|| "none".to_string())
    )
}

fn shared_http_client(options: &Dynamic) -> Result<std::sync::Arc<reqwest::Client>> {
    static CLIENTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<reqwest::Client>>>> = std::sync::OnceLock::new();

    let key = http_client_cache_key(options);
    let clients = CLIENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut clients = clients.lock().map_err(|_| anyhow!("LLM HTTP client cache lock poisoned"))?;
    if let Some(client) = clients.get(&key) {
        return Ok(client.clone());
    }
    let client = std::sync::Arc::new(http_client_builder(options).build()?);
    clients.insert(key, client.clone());
    Ok(client)
}

fn stream_tail_preview(text: &str, max_chars: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }
    text.chars().skip(total_chars - max_chars).collect()
}

fn validate_stream_finish_reason(finish_reason: Option<&str>, text: &str) -> Result<()> {
    match finish_reason {
        None | Some("") | Some("stop") | Some("tool_calls") => Ok(()),
        Some(reason) => Err(anyhow!("LLM 生成未完整结束: finish_reason={reason}; received_chars={}; tail={:?}", text.chars().count(), stream_tail_preview(text, 2000))),
    }
}

fn validate_non_stream_finish_reason(body: &Dynamic, raw_text: &str) -> Result<()> {
    let reason = body.get_dynamic("choices").and_then(|choices| choices.get_idx(0)).and_then(|choice| choice.get_dynamic("finish_reason")).map(|value| value.as_str().to_string());
    validate_stream_finish_reason(reason.as_deref(), raw_text)
}

fn copy_request_options_except(options: &Dynamic, msg: &Dynamic, skipped_keys: &[&str]) {
    for key in options.keys() {
        if key != "url"
            && key != "key"
            && key != "api_key"
            && key != "apiKey"
            && key != "token"
            && key != "access_key"
            && key != "accessKey"
            && key != "access_id"
            && key != "accessId"
            && key != "ak"
            && key != "secret_key"
            && key != "secretKey"
            && key != "access_secret"
            && key != "accessSecret"
            && key != "sk"
            && key != "api"
            && key != "endpoint"
            && key != "method"
            && key != "kind"
            && key != "provider"
            && key != "debug_log_file"
            && key != "no_proxy"
            && key != "connect_timeout_ms"
            && key != "read_timeout_ms"
            && key != "request_timeout_ms"
            && key != "timeout_ms"
            && key != "name"
            && key != "brand"
            && key != "text_model"
            && key != "vision_model"
            && key != "image_model"
            && key != "audio_model"
            && key != "tts_model"
            && key != "asr_model"
            && !skipped_keys.iter().any(|skipped| key.as_str() == *skipped)
        {
            msg.insert(key.clone(), options.get_dynamic(key.as_str()).unwrap());
        }
    }
}

pub async fn post_binary(method: &str, openai: Dynamic, msg: Dynamic) -> Result<Dynamic> {
    let url = openai.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let token = bearer_token(&openai)?;

    copy_request_options_except(&openai, &msg, &["stream"]);

    let client = shared_http_client(&openai)?;
    let mut body_str = String::new();
    msg.to_json(&mut body_str);
    log::info!("{}", body_str);
    debug_log_event(
        &openai,
        map!(
            "event"=> "llm_http_request",
            "method"=> method,
            "url"=> format!("{}/{}", url.as_str(), method),
            "body"=> body_str.clone()
        ),
    );

    let resp = if let Some(token) = token {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").header("authorization", format!("Bearer {}", token)).body(body_str).send().await?
    } else {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").body(body_str).send().await?
    };
    let status = resp.status();
    let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
    let bytes = resp.bytes().await?;

    if !status.is_success() {
        let text = String::from_utf8_lossy(&bytes);
        return Err(anyhow!("LLM 请求失败 HTTP {}: {}", status.as_u16(), text.trim()));
    }

    decode_binary_response(bytes.to_vec(), &content_type)
}

fn decode_binary_response(bytes: Vec<u8>, content_type: &str) -> Result<Dynamic> {
    if content_type.starts_with("application/json") {
        let (body, _) = Dynamic::from_json(&bytes)?;
        return decode_tts_json_response(body);
    }

    Ok(Dynamic::Bytes(bytes))
}

fn decode_tts_json_response(body: Dynamic) -> Result<Dynamic> {
    for key in ["audio", "data", "b64_json", "base64", "audio_base64"] {
        if let Some(value) = body.get_dynamic(key) {
            if value.as_bytes().is_some() {
                return Ok(value);
            }
            if value.is_str() {
                return general_purpose::STANDARD.decode(value.as_str()).map(Dynamic::Bytes).map_err(|e| anyhow!("TTS 响应字段 {key} 不是合法 base64: {e}"));
            }
        }
    }

    if let Some(url) = body.get_dynamic("url").or_else(|| body.get_dynamic("audio_url")) {
        return Ok(map!("url"=> url));
    }

    for key in ["data", "result"] {
        if let Some(value) = body.get_dynamic(key) {
            if value.is_map() {
                return decode_tts_json_response(value);
            }
            if value.is_list() {
                if let Some(item) = value.get_idx(0) {
                    return decode_tts_json_response(item);
                }
            }
        }
    }

    Ok(body)
}

pub async fn post(method: &str, openai: Dynamic, msg: Dynamic, tx: Option<Dynamic>) -> Result<Dynamic> {
    let max_attempts = openai.get_dynamic("retry_attempts").and_then(|value| value.as_int()).unwrap_or(3).clamp(1, 5) as usize;

    for attempt in 0..max_attempts {
        match post_once(method, openai.clone(), msg.clone(), tx.clone()).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                let detail = format!("{err:#}");
                let retryable = ["error sending request", "connection error", "tls handshake", "unexpected-eof", "unexpected eof", "peer closed connection", "connection reset", "timed out"]
                    .iter()
                    .any(|pattern| detail.to_ascii_lowercase().contains(pattern));
                if !retryable || attempt + 1 >= max_attempts {
                    return Err(err);
                }
                let delay_ms = 500u64 * (1u64 << attempt);
                log::warn!("LLM transport failed, retrying attempt {}/{} in {}ms: {}", attempt + 2, max_attempts, delay_ms, err);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
    unreachable!("LLM retry loop always returns")
}

async fn post_once(method: &str, openai: Dynamic, msg: Dynamic, tx: Option<Dynamic>) -> Result<Dynamic> {
    //不能把 dynamic 作为耗材使用
    let url = openai.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let token = bearer_token(&openai)?;

    let is_stream = openai.get_dynamic("stream").and_then(|value| value.as_bool()).unwrap_or(tx.is_some());
    openai.insert("stream", is_stream);

    copy_request_options(&openai, &msg);

    normalize_glm_tools(&msg);

    // 进程内复用 reqwest Client 的连接池；显式失败恢复会启动新进程，
    // 因而不会把坏连接池带入下一次 run，也不在同一 run 自动重试。
    let client = shared_http_client(&openai)?;
    let mut body_str = String::new();
    msg.to_json(&mut body_str);
    log::info!("{}", body_str);

    let resp = if let Some(token) = token {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").header("authorization", format!("Bearer {}", token)).body(body_str).send().await?
    } else {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").body(body_str).send().await?
    };
    let status = resp.status();

    let stream = resp.bytes_stream().map(|result| result.map_err(|e| tokio::io::Error::new(tokio::io::ErrorKind::Other, e)));
    use tokio::io::AsyncBufReadExt;
    let reader = StreamReader::new(stream);
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    let mut text = String::new();
    let mut finish_reason: Option<String> = None;
    loop {
        line.clear(); // 清空上一行的内容
        if buf_reader.read_line(&mut line).await? == 0 {
            break;
        }
        if is_stream && line.trim() == "data: [DONE]" {
            // SSE 已明确完成；不要继续等服务端关闭 chunked/TLS 连接。
            break;
        }
        if is_stream && line.trim().starts_with("data") {
            if let Some(start) = line.find('{') {
                if let Ok((v, _)) = Dynamic::from_json(&line.as_bytes()[start..]) {
                    if let Some(choice) = v.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|choices| choices.into_iter().next()) {
                        if let Some(reason) = choice.get_dynamic("finish_reason") {
                            let reason = reason.as_str();
                            if !reason.is_empty() {
                                finish_reason = Some(reason.to_string());
                            }
                        }
                        if let Some(delta) = choice.remove_dynamic("delta") {
                            // 思考链（OpenAI o1 用 reasoning_content；Claude/DeepSeek 用 thinking 或 reasoning）
                            for key in ["reasoning_content", "thinking", "reasoning"] {
                                if let Some(think) = delta.get_dynamic(key) {
                                    let s = think.as_str();
                                    if !s.is_empty() {
                                        tx.as_ref().map(|tx| {
                                            log::debug!("think delta: {:?}", &s);
                                            notify(tx, map!("think"=> s))
                                        });
                                    }
                                }
                            }
                            // 正文
                            if let Some(content) = delta.get_dynamic("content") {
                                let s = content.as_str();
                                if !s.is_empty() {
                                    text.push_str(s);
                                    // Opt-in raw stream visibility for supervised probe runs.
                                    // Keep the default quiet so normal callers and persisted
                                    // artifacts are unchanged; stderr can be watched via the
                                    // caller's existing run log/terminal.
                                    if std::env::var_os("ZUST_LLM_STREAM_STDERR").is_some() {
                                        use std::io::Write as _;
                                        eprint!("{s}");
                                        let _ = std::io::stderr().flush();
                                    }
                                    tx.as_ref().map(|tx| {
                                        log::debug!("text delta: {:?}", &s);
                                        notify(tx, map!("text"=> s))
                                    });
                                }
                            }
                        }
                    }
                }
            }
        } else if !is_stream {
            // SSE 的 data 事件之间有空分隔行；streaming 模式必须忽略，
            // 否则会在每个 token 后错误插入换行。
            text.push_str(&line);
        }
    }
    if is_stream {
        tx.as_ref().map(|tx| notify(tx, Dynamic::Null));
    }
    log::info!("{:#?}", &text);
    debug_log_event(
        &openai,
        map!(
            "event"=> "llm_http_response",
            "method"=> method,
            "url"=> format!("{}/{}", url.as_str(), method),
            "status"=> status.as_u16() as i64,
            "body"=> text.clone()
        ),
    );
    if !status.is_success() {
        return Err(anyhow!("LLM 请求失败 HTTP {}: {}", status.as_u16(), text.trim()));
    }
    if is_stream {
        validate_stream_finish_reason(finish_reason.as_deref(), &text)?;
        return decode_text_content(Dynamic::from(text.clone()), text.trim());
    }
    let (t, _) = Dynamic::from_json(text.as_bytes()).map_err(|e| {
        tx.as_ref().map(|tx| notify(tx, Dynamic::Null));
        anyhow!("LLM 响应不是合法 JSON: {e}; body: {}", text.trim())
    })?;

    decode_llm_response(t, text.trim())
}

/// 流式事件：thinking 链、正文 token、或完成。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Thinking(String),
    Content(String),
    Done,
    Error(String),
}

/// Rust 侧的流式接口（不走 root::send，直接回调）。
/// 自动设置 stream=true，逐块抽 reasoning_content/thinking/reasoning 进 Thinking、content 进 Content。
pub async fn stream<F>(bigmodel: Dynamic, msg: Dynamic, mut on_event: F) -> Result<()>
where
    F: FnMut(StreamEvent) + Send,
{
    let bigmodel = with_kind_model(bigmodel, "complete")?;
    // 强制开 stream
    bigmodel.insert("stream", true);

    // 走 chat/completions（responses API 暂不支持流式）
    let body = if msg.is_map() && msg.contains("messages") { msg } else { map!("messages"=> list!(map!("role"=> "user", "content"=> chat_content(msg)))) };

    let url = bigmodel.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let token = bearer_token(&bigmodel)?;
    copy_request_options(&bigmodel, &body);
    normalize_glm_tools(&body);

    let client = shared_http_client(&bigmodel)?;
    let mut body_str = String::new();
    body.to_json(&mut body_str);
    log::debug!("stream req: {}", body_str);

    let resp = if let Some(token) = token {
        client.post(&format!("{}/chat/completions", url.as_str())).header("Content-Type", "application/json").header("authorization", format!("Bearer {}", token)).body(body_str).send().await?
    } else {
        client.post(&format!("{}/chat/completions", url.as_str())).header("Content-Type", "application/json").body(body_str).send().await?
    };
    let status = resp.status();

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let err = format!("LLM HTTP {}: {}", status.as_u16(), text.trim());
        on_event(StreamEvent::Error(err.clone()));
        return Err(anyhow!(err));
    }

    let stream = resp.bytes_stream().map(|r| r.map_err(|e| tokio::io::Error::new(tokio::io::ErrorKind::Other, e)));
    use tokio::io::AsyncBufReadExt;
    let reader = StreamReader::new(stream);
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        if buf_reader.read_line(&mut line).await? == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("data") {
            continue;
        }
        // data: [DONE] 表示结束
        if trimmed.contains("[DONE]") {
            break;
        }
        let Some(start) = line.find('{') else {
            continue;
        };
        let Ok((v, _)) = Dynamic::from_json(&line.as_bytes()[start..]) else {
            continue;
        };

        let Some(choice) = v.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|choices| choices.into_iter().next()) else {
            continue;
        };
        let Some(delta) = choice.remove_dynamic("delta") else {
            continue;
        };

        // thinking 链
        for key in ["reasoning_content", "thinking", "reasoning"] {
            if let Some(think) = delta.get_dynamic(key) {
                let s = think.as_str();
                if !s.is_empty() {
                    on_event(StreamEvent::Thinking(s.to_string()));
                }
            }
        }
        // 正文
        if let Some(content) = delta.get_dynamic("content") {
            let s = content.as_str();
            if !s.is_empty() {
                on_event(StreamEvent::Content(s.to_string()));
            }
        }
    }

    on_event(StreamEvent::Done);
    Ok(())
}

fn decode_llm_response(t: Dynamic, raw_text: &str) -> Result<Dynamic> {
    validate_non_stream_finish_reason(&t, raw_text)?;
    if let Some(data) = t.get_dynamic("data") {
        if data.is_list()
            && let Some(item) = data.get_idx(0)
        {
            return Ok(item);
        }
        if data.is_map() && ["task_id", "url", "image_url", "image", "output_url", "b64_json", "base64", "audio_url", "task_result"].iter().any(|key| data.contains(*key)) {
            return Ok(data);
        }
    }
    if let Some(output) = t.get_dynamic("output")
        && let Some(decoded) = decode_output(output)
    {
        return Ok(decoded);
    }
    if let Some(content) = decode_responses_text(&t) {
        return decode_text_content(content, raw_text);
    }
    let choice = t.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|v| v.into_iter().next()).ok_or_else(|| anyhow!("LLM 响应缺少 data[0] 或 choices[0]: {raw_text}"))?;
    if let Some(message) = choice.remove_dynamic("message") {
        if message.contains("tool_calls") {
            return Ok(message);
        }
        if let Some(content) = message.remove_dynamic("content") {
            return decode_text_content(content, raw_text);
        }
    }
    Err(anyhow!("结果不是 json"))
}

fn decode_output(output: Dynamic) -> Option<Dynamic> {
    if output.is_list() {
        for idx in 0..output.len() {
            let item = output.get_idx(idx)?;
            if let Some(decoded) = decode_output(item) {
                return Some(decoded);
            }
        }
        return None;
    }

    if !output.is_map() {
        return None;
    }

    for key in ["task_id", "url", "image_url", "image", "output_url", "b64_json", "base64", "audio_url"] {
        if output.contains(key) {
            return Some(output);
        }
    }

    if let Some(choices) = output.get_dynamic("choices")
        && choices.is_list()
    {
        for idx in 0..choices.len() {
            let choice = choices.get_idx(idx)?;
            if let Some(message) = choice.get_dynamic("message")
                && let Some(content) = message.get_dynamic("content")
                && let Some(decoded) = decode_output_content(content)
            {
                return Some(decoded);
            }
        }
    }

    if let Some(results) = output.get_dynamic("results")
        && let Some(decoded) = decode_output(results)
    {
        return Some(decoded);
    }

    None
}

fn decode_output_content(content: Dynamic) -> Option<Dynamic> {
    if content.is_list() {
        for idx in 0..content.len() {
            let item = content.get_idx(idx)?;
            if let Some(decoded) = decode_output_content(item) {
                return Some(decoded);
            }
        }
        return None;
    }

    if content.is_map() {
        for key in ["image", "image_url", "url", "output_url", "text"] {
            if content.contains(key) {
                return Some(content);
            }
        }
    }

    None
}

fn decode_responses_text(t: &Dynamic) -> Option<Dynamic> {
    let output = t.get_dynamic("output")?;
    if !output.is_list() {
        return None;
    }

    for idx in 0..output.len() {
        let item = output.get_idx(idx)?;
        let content = item.get_dynamic("content")?;
        if !content.is_list() {
            continue;
        }
        for content_idx in 0..content.len() {
            let content_item = content.get_idx(content_idx)?;
            if content_item.get_dynamic("type").is_some_and(|ty| ty.as_str() == "output_text")
                && let Some(text) = content_item.get_dynamic("text")
            {
                return Some(text);
            }
        }
    }

    None
}

pub fn decode_text_content(content: Dynamic, raw_text: &str) -> Result<Dynamic> {
    decode_text_content_with_yaml_policy(content, raw_text, std::env::var_os("ZUST_LLM_PRESERVE_FENCED_YAML").is_some())
}

fn decode_text_content_with_yaml_policy(content: Dynamic, _raw_text: &str, preserve_fenced_yaml: bool) -> Result<Dynamic> {
    let mut text = content.as_str().to_string();
    let mut think_text = String::new();
    loop {
        let lower = text.to_ascii_lowercase();
        let Some(start) = lower.find("<think>") else {
            break;
        };
        let body_start = start + "<think>".len();
        if let Some(end_offset) = lower[body_start..].find("</think>") {
            let end = body_start + end_offset;
            let think = text[body_start..end].trim();
            if !think.is_empty() {
                if !think_text.is_empty() {
                    think_text.push('\n');
                }
                think_text.push_str(think);
            }
            text.replace_range(start..end + "</think>".len(), "");
        } else {
            let think = text[body_start..].trim();
            if !think.is_empty() {
                if !think_text.is_empty() {
                    think_text.push('\n');
                }
                think_text.push_str(think);
            }
            text.truncate(start);
            break;
        }
    }
    if !think_text.is_empty() {
        log::info!("LLM think:\n{}", think_text);
    }
    let text = text.trim();
    // Some chat models occasionally duplicate the opening YAML fence while
    // emitting only one closing fence:
    //
    // ```yaml
    // ```yaml
    // key: value
    // ```
    //
    // Without this normalization the generic fenced-block regex pairs the
    // two opening fences and decodes an empty YAML document, discarding the
    // real payload. Collapse only a consecutive duplicate YAML/YML opening
    // fence at the very start; do not alter payload text or other languages.
    let duplicate_yaml_open = regex::Regex::new(r"(?i)^```[ \t]*(?:yaml|yml)[ \t]*\r?\n[ \t]*```[ \t]*(?:yaml|yml)[ \t]*\r?\n")?;
    let normalized_text = if let Some(matched) = duplicate_yaml_open.find(text) { format!("```yaml\n{}", &text[matched.end()..]) } else { text.to_owned() };
    let text = normalized_text.trim();
    let reg = regex::Regex::new(r"(?s)```[ \t]*([^\r\n]*)\r?\n(.*?)\r?\n?[ \t]*```")?;
    if let Some(cap) = reg.captures_iter(text).next() {
        let format = cap.get(1).map_or("", |value| value.as_str()).trim().to_ascii_lowercase();
        let code = cap.get(2).unwrap().as_str().trim();
        if format == "json" || (format.is_empty() && ((code.starts_with('{') && code.ends_with('}')) || (code.starts_with('[') && code.ends_with(']')))) {
            Dynamic::from_json(code.as_bytes()).map(|(value, _)| value)
        } else if format == "yaml" || format == "yml" {
            if preserve_fenced_yaml { Ok(map!("lang" => "yaml", "code" => code)) } else { Dynamic::from_yaml(code.as_bytes()).map(|(value, _)| value) }
        } else {
            Ok(code.into())
        }
    } else if (text.starts_with('{') && text.ends_with('}')) || (text.starts_with('[') && text.ends_with(']')) {
        Dynamic::from_json(text.as_bytes()).map(|(value, _)| value)
    } else {
        Ok(text.into())
    }
}

#[cfg(test)]
mod test {
    use base64::{Engine as _, engine::general_purpose};
    use dynamic::{Dynamic, FromJson};
    use dynamic::{list, map};
    use hmac::Mac;

    use super::HmacSha256;

    #[test]
    fn normalize_tool_adds_required_type() {
        let body = map!(
            "tools"=> list!(
                map!(
                    "function"=> map!(
                        "name"=> "lookup",
                        "description"=> "lookup data",
                        "parameters"=> map!("type"=> "object", "properties"=> map!())
                    )
                )
            )
        );

        super::normalize_glm_tools(&body);

        let tool = body.get_dynamic("tools").unwrap().get_idx(0).unwrap();
        assert_eq!(tool.get_dynamic("type").unwrap().as_str(), "function");
        assert!(tool.get_dynamic("function").unwrap().contains("parameters"));
    }

    #[test]
    fn normalize_responses_function_tool_to_glm_shape() {
        let body = map!(
            "tools"=> list!(
                map!(
                    "type"=> "function",
                    "name"=> "lookup",
                    "description"=> "lookup data",
                    "parameters"=> map!("type"=> "object", "properties"=> map!()),
                    "strict"=> true
                )
            )
        );

        super::normalize_glm_tools(&body);

        let tool = body.get_dynamic("tools").unwrap().get_idx(0).unwrap();
        assert_eq!(tool.get_dynamic("type").unwrap().as_str(), "function");
        let function = tool.get_dynamic("function").unwrap();
        assert_eq!(function.get_dynamic("name").unwrap().as_str(), "lookup");
        assert!(function.get_dynamic("strict").unwrap().is_true());
        assert!(tool.get_dynamic("name").is_none());
    }

    #[test]
    fn normalize_tools_caps_glm_tool_count() {
        let mut tools = Vec::new();
        for idx in 0..12 {
            tools.push(map!(
                "type"=> "function",
                "name"=> format!("tool_{}", idx),
                "parameters"=> map!("type"=> "object", "properties"=> map!())
            ));
        }
        let body = map!("tools"=> Dynamic::list(tools));

        super::normalize_glm_tools(&body);

        let tools = body.get_dynamic("tools").unwrap();
        assert_eq!(tools.len(), 9);
        let last_tool = tools.get_idx(8).unwrap();
        let function = last_tool.get_dynamic("function").unwrap();
        assert_eq!(function.get_dynamic("name").unwrap().as_str(), "tool_8");
    }

    #[test]
    fn decode_embedding_response_returns_first_data_item() -> anyhow::Result<()> {
        let raw = r#"{"data":[{"embedding":[1.0,2.0,3.0]}]}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let item = super::decode_llm_response(body, raw)?;
        let embedding = item.get_dynamic("embedding").expect("embedding");

        assert_eq!(embedding.len(), 3);
        Ok(())
    }

    #[test]
    fn decode_unexpected_response_reports_original_body() -> anyhow::Result<()> {
        let raw = r#"{"error":{"message":"bad api key"}}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let err = super::decode_llm_response(body, raw).expect_err("unexpected body should fail");
        let message = err.to_string();

        assert!(message.contains("缺少 data[0] 或 choices[0]"));
        assert!(message.contains("bad api key"));
        Ok(())
    }

    #[test]
    fn decode_chat_tool_calls_returns_message() -> anyhow::Result<()> {
        let raw = r#"{"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"search_google_maps","arguments":"{\"address\":\"大阪府大阪市北区梅田3丁目1-1\"}"}}]}}]}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let decoded = super::decode_llm_response(body, raw)?;

        assert!(decoded.contains("tool_calls"));
        assert_eq!(decoded.get_dynamic("role").unwrap().as_str(), "assistant");
        let tool_calls = decoded.get_dynamic("tool_calls").unwrap();
        assert_eq!(tool_calls.len(), 1);
        let call = tool_calls.get_idx(0).unwrap();
        assert_eq!(call.get_dynamic("function").unwrap().get_dynamic("name").unwrap().as_str(), "search_google_maps");
        Ok(())
    }

    #[test]
    fn decode_tts_json_response_accepts_base64_audio() -> anyhow::Result<()> {
        let body = map!("audio"=> "AQID");
        let audio = super::decode_tts_json_response(body)?;

        assert_eq!(audio.as_bytes(), Some(&[1, 2, 3][..]));
        Ok(())
    }

    #[test]
    fn decode_tts_json_response_keeps_audio_url() -> anyhow::Result<()> {
        let body = map!("audio_url"=> "https://example.test/audio.mp3");
        let audio = super::decode_tts_json_response(body)?;

        assert_eq!(audio.get_dynamic("url").unwrap().as_str(), "https://example.test/audio.mp3");
        Ok(())
    }

    #[test]
    fn decode_tts_json_response_accepts_nested_data_item() -> anyhow::Result<()> {
        let body = map!("data"=> list!(map!("url"=> "https://example.test/audio.wav")));
        let audio = super::decode_tts_json_response(body)?;

        assert_eq!(audio.get_dynamic("url").unwrap().as_str(), "https://example.test/audio.wav");
        Ok(())
    }

    #[test]
    fn responses_api_detects_input_file_content() {
        let openai = map!("url"=> "https://example.test", "model"=> "doubao");
        let msg = list!(map!("type"=> "input_file", "file_url"=> "https://example.test/doc.pdf"), map!("type"=> "input_text", "text"=> "extract"));

        assert!(super::uses_responses_api(&openai, &msg));
    }

    #[test]
    fn responses_api_can_be_configured_explicitly() {
        let openai = map!("url"=> "https://example.test", "api"=> "responses");

        assert!(super::uses_responses_api(&openai, &"plain text".into()));
    }

    #[test]
    fn chat_content_preserves_typed_items() {
        let item = map!("type"=> "image_url", "image_url"=> map!("url"=> "https://example.test/a.jpg"));
        let normalized = super::chat_content_item(item);

        assert_eq!(normalized.get_dynamic("type").unwrap().as_str(), "image_url");
        assert!(normalized.get_dynamic("text").is_none());
    }

    #[test]
    fn chat_content_turns_image_urls_into_image_items() {
        let normalized = super::chat_content_item("https://example.test/a.jpg".into());

        assert_eq!(normalized.get_dynamic("type").unwrap().as_str(), "image_url");
        assert_eq!(normalized.get_dynamic("image_url").unwrap().get_dynamic("url").unwrap().as_str(), "https://example.test/a.jpg");
        assert!(normalized.get_dynamic("text").is_none());
    }

    #[test]
    fn chat_content_keeps_plain_text_as_text_item() {
        let normalized = super::chat_content_item("hello".into());

        assert_eq!(normalized.get_dynamic("type").unwrap().as_str(), "text");
        assert_eq!(normalized.get_dynamic("text").unwrap().as_str(), "hello");
        assert!(normalized.get_dynamic("image_url").is_none());
    }

    #[test]
    fn audio_data_url_is_sent_as_base64_data() {
        let audio = map!("url"=> "data:audio/wav;base64,AQID");
        let normalized = super::normalize_audio_payload(audio).unwrap();

        assert_eq!(normalized.get_dynamic("data").unwrap().as_str(), "AQID");
        assert!(normalized.get_dynamic("url").is_none());
    }

    #[test]
    fn decode_responses_output_text_json() -> anyhow::Result<()> {
        let raw = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"{\"ok\":true,\"name\":\"pdf\"}"}]}]}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let decoded = super::decode_llm_response(body, raw)?;

        assert!(decoded.get_dynamic("ok").unwrap().is_true());
        assert_eq!(decoded.get_dynamic("name").unwrap().as_str(), "pdf");
        Ok(())
    }

    #[test]
    fn decode_text_content_strips_think_before_json() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("<think>check shape</think>\n{\"ok\":true}".into(), "")?;

        assert!(decoded.get_dynamic("ok").unwrap().is_true());
        Ok(())
    }

    #[test]
    fn decode_text_content_strips_unclosed_think_from_plain_text() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("answer\n<think>unfinished".into(), "")?;

        assert_eq!(decoded.as_str(), "answer");
        Ok(())
    }

    #[test]
    fn decode_text_content_keeps_yaml_with_rust_braces_as_text() -> anyhow::Result<()> {
        let yaml = "path_role: benchmark\nsource_units:\n  - signature: \"macro_rules! generate { ($name:ident) => { ... } }\"\noutput_complete: true";
        let decoded = super::decode_text_content(yaml.into(), "")?;

        assert!(decoded.is_str());
        assert_eq!(decoded.as_str(), yaml);
        Ok(())
    }

    #[test]
    fn decode_text_content_decodes_fenced_json() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("result:\n```json\n{\"ok\": true}\n```".into(), "")?;

        assert!(decoded.get_dynamic("ok").unwrap().is_true());
        Ok(())
    }

    #[test]
    fn decode_text_content_decodes_fenced_yaml() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("```yaml\nok: true\nitems:\n  - one\n  - two\n```".into(), "")?;

        assert!(decoded.get_dynamic("ok").unwrap().is_true());
        assert_eq!(decoded.get_dynamic("items").unwrap().len(), 2);
        Ok(())
    }

    #[test]
    fn decode_text_content_can_preserve_fenced_yaml_wire() -> anyhow::Result<()> {
        let decoded = super::decode_text_content_with_yaml_policy("```yaml\nsemantic_items:\n  \"first\": \"第一条语义。\n  \"second\": \"第二条语义。\n```".into(), "", true)?;
        assert_eq!(decoded.get_dynamic("lang").unwrap().as_str(), "yaml");
        let code_value = decoded.get_dynamic("code").unwrap();
        let code = code_value.as_str();
        assert!(code.contains("\"first\": \"第一条语义。"));
        assert!(code.contains("\"second\": \"第二条语义。"));
        Ok(())
    }

    #[test]
    fn decode_text_content_decodes_duplicated_yaml_opening_fence() -> anyhow::Result<()> {
        for input in ["```yaml\n```yaml\nok: true\nitems:\n  - one\n```", "```YAML\r\n```yml\r\nok: true\r\nitems:\r\n  - one\r\n```"] {
            let decoded = super::decode_text_content(input.into(), "")?;
            assert!(decoded.get_dynamic("ok").unwrap().is_true());
            assert_eq!(decoded.get_dynamic("items").unwrap().len(), 1);
        }
        Ok(())
    }

    #[test]
    fn decode_text_content_returns_unknown_fence_as_string() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("```rust\nfn main() {}\n```".into(), "")?;

        assert!(decoded.is_str());
        assert_eq!(decoded.as_str(), "fn main() {}");
        Ok(())
    }

    #[test]
    fn decode_text_content_decodes_trimmed_json_array() -> anyhow::Result<()> {
        let decoded = super::decode_text_content("  [1, 2, 3]\n".into(), "")?;

        assert!(decoded.is_list());
        assert_eq!(decoded.len(), 3);
        Ok(())
    }

    #[test]
    fn decode_text_content_keeps_embedded_json_as_string() -> anyhow::Result<()> {
        let text = "result is {\"ok\": true}";
        let decoded = super::decode_text_content(text.into(), "")?;

        assert!(decoded.is_str());
        assert_eq!(decoded.as_str(), text);
        Ok(())
    }

    #[test]
    fn normalize_provider_requires_explicit_url() {
        let options = map!("model"=> "qwen-plus", "key"=> "sk-test");
        let err = super::normalize_provider(options).expect_err("missing url should fail");

        assert!(err.to_string().contains("url"), "error should mention url: {err}");
    }

    #[test]
    fn normalize_provider_rejects_empty_url() {
        let options = map!("url"=> "  ", "model"=> "qwen-plus");
        let err = super::normalize_provider(options).expect_err("blank url should fail");

        assert!(err.to_string().contains("url"), "error should mention url: {err}");
    }

    #[test]
    fn normalize_provider_keeps_user_url_without_inference() -> anyhow::Result<()> {
        let options = map!("url"=> "https://my-proxy.test/v1", "model"=> "qwen-plus", "key"=> "sk-test");
        let normalized = super::normalize_provider(options)?;

        assert_eq!(normalized.get_dynamic("url").unwrap().as_str(), "https://my-proxy.test/v1");
        Ok(())
    }

    #[test]
    fn with_kind_model_requires_explicit_model() {
        let options = map!("url"=> "https://example.test", "key"=> "sk-test");
        let err = super::with_kind_model(options, "complete").expect_err("missing model should fail");

        assert!(err.to_string().contains("model"), "error should mention model: {err}");
    }

    #[test]
    fn with_kind_model_rejects_empty_model() {
        let options = map!("url"=> "https://example.test", "model"=> "");
        let err = super::with_kind_model(options, "complete").expect_err("blank model should fail");

        assert!(err.to_string().contains("model"), "error should mention model: {err}");
    }

    #[test]
    fn with_kind_model_promotes_kind_specific_model() -> anyhow::Result<()> {
        let options = map!("url"=> "https://example.test", "model"=> "deepseek-chat", "vision_model"=> "gpt-4o");
        let normalized = super::with_kind_model(options, "complete")?;

        assert_eq!(normalized.get_dynamic("model").unwrap().as_str(), "gpt-4o");
        Ok(())
    }

    #[test]
    fn with_kind_model_image_kind_uses_image_model() -> anyhow::Result<()> {
        let options = map!("url"=> "https://example.test", "image_model"=> "dall-e-3");
        let normalized = super::with_kind_model(options, "image")?;

        assert_eq!(normalized.get_dynamic("model").unwrap().as_str(), "dall-e-3");
        Ok(())
    }

    #[test]
    fn dashscope_image_body_uses_existing_engine_shape() {
        let body = super::dashscope_image_body(map!(
            "prompt"=> "extend",
            "image"=> "https://example.test/base.jpg",
            "referenceImages"=> list!("https://example.test/ref.png")
        ));
        let messages = body.get_dynamic("input").unwrap().get_dynamic("messages").unwrap();
        let content = messages.get_idx(0).unwrap().get_dynamic("content").unwrap();

        assert_eq!(content.get_idx(0).unwrap().get_dynamic("image").unwrap().as_str(), "https://example.test/base.jpg");
        assert_eq!(content.get_idx(1).unwrap().get_dynamic("image").unwrap().as_str(), "https://example.test/ref.png");
        assert_eq!(content.get_idx(2).unwrap().get_dynamic("text").unwrap().as_str(), "extend");
    }

    #[test]
    fn kling_image_expand_body_uses_expansion_shape() {
        let body = super::kling_image_expand_body(map!(
            "text"=> "extend right",
            "image_url"=> "https://example.test/base.jpg",
            "right_expansion_ratio"=> 0.5
        ));

        assert_eq!(body.get_dynamic("image").unwrap().as_str(), "https://example.test/base.jpg");
        assert_eq!(body.get_dynamic("prompt").unwrap().as_str(), "extend right");
        assert_eq!(body.get_dynamic("n").unwrap().as_int().unwrap(), 1);
        assert_eq!(body.get_dynamic("right_expansion_ratio").unwrap().as_float().unwrap(), 0.5);
        assert_eq!(body.get_dynamic("left_expansion_ratio").unwrap().as_int().unwrap(), 0);
    }

    #[test]
    fn kling_image_generation_body_uses_generation_shape() {
        let body = super::kling_image_generation_body(map!(
            "text"=> "new village",
            "model"=> "kling-v2-1",
            "aspect_ratio"=> "9:16",
            "resolution"=> "2k"
        ));

        assert_eq!(body.get_dynamic("prompt").unwrap().as_str(), "new village");
        assert_eq!(body.get_dynamic("model_name").unwrap().as_str(), "kling-v2-1");
        assert_eq!(body.get_dynamic("aspect_ratio").unwrap().as_str(), "9:16");
        assert_eq!(body.get_dynamic("resolution").unwrap().as_str(), "2k");
        assert_eq!(body.get_dynamic("n").unwrap().as_int().unwrap(), 1);
        assert!(body.get_dynamic("model").is_none());
    }

    #[test]
    fn kling_provider_accepts_singapore_domain() {
        let options = map!("url"=> "https://api-singapore.klingai.com/v1/images/generations");

        assert!(super::uses_kling_image_generation(&options));
    }

    #[test]
    fn kling_bearer_token_uses_ak_sk_jwt() -> anyhow::Result<()> {
        let options = map!(
            "kind"=> "kling_image_generation",
            "access_key"=> "ak-test",
            "secret_key"=> "sk-test"
        );
        let token = super::bearer_token(&options)?.expect("token");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        let header = general_purpose::URL_SAFE_NO_PAD.decode(parts[0])?;
        let payload = general_purpose::URL_SAFE_NO_PAD.decode(parts[1])?;
        let (header, _) = Dynamic::from_json(&header)?;
        let (payload, _) = Dynamic::from_json(&payload)?;

        assert_eq!(header.get_dynamic("alg").unwrap().as_str(), "HS256");
        assert_eq!(header.get_dynamic("typ").unwrap().as_str(), "JWT");
        assert_eq!(payload.get_dynamic("iss").unwrap().as_str(), "ak-test");
        assert!(payload.get_dynamic("exp").unwrap().as_int().unwrap() > payload.get_dynamic("nbf").unwrap().as_int().unwrap());

        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let mut mac = HmacSha256::new_from_slice(b"sk-test")?;
        mac.update(signing_input.as_bytes());
        let expected = general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        assert_eq!(parts[2], expected);
        Ok(())
    }

    #[test]
    fn copy_request_options_does_not_copy_auth_secrets() {
        let options = map!(
            "url"=> "https://api-singapore.klingai.com",
            "access_key"=> "ak-test",
            "secret_key"=> "sk-test",
            "no_proxy"=> true,
            "connect_timeout_ms"=> 30000,
            "read_timeout_ms"=> 120000,
            "model"=> "kling-v2-1"
        );
        let body = map!("prompt"=> "village");

        super::copy_request_options(&options, &body);

        assert!(body.get_dynamic("access_key").is_none());
        assert!(body.get_dynamic("secret_key").is_none());
        assert!(body.get_dynamic("no_proxy").is_none());
        assert!(body.get_dynamic("connect_timeout_ms").is_none());
        assert!(body.get_dynamic("read_timeout_ms").is_none());
        assert_eq!(body.get_dynamic("model").unwrap().as_str(), "kling-v2-1");
    }

    #[test]
    fn no_proxy_option_is_explicit_boolean() {
        assert!(super::no_proxy_enabled(&map!("no_proxy"=> true)));
        assert!(!super::no_proxy_enabled(&map!("no_proxy"=> false)));
        assert!(!super::no_proxy_enabled(&map!()));
    }

    #[test]
    fn transport_timeout_options_are_milliseconds() {
        let options = map!("read_timeout_ms"=> 45000);
        assert_eq!(super::duration_option_ms(&options, &["read_timeout_ms"]).unwrap(), std::time::Duration::from_secs(45));
        assert!(super::duration_option_ms(&options, &["request_timeout_ms"]).is_none());
    }

    #[test]
    fn shared_http_client_reuses_only_matching_transport_config() -> anyhow::Result<()> {
        let first_options = map!("no_proxy"=> true, "connect_timeout_ms"=> 30000, "read_timeout_ms"=> 120000);
        let same_options = first_options.clone();
        let different_options = map!("no_proxy"=> true, "connect_timeout_ms"=> 30000, "read_timeout_ms"=> 121000);

        let first = super::shared_http_client(&first_options)?;
        let same = super::shared_http_client(&same_options)?;
        let different = super::shared_http_client(&different_options)?;

        assert!(std::sync::Arc::ptr_eq(&first, &same));
        assert!(!std::sync::Arc::ptr_eq(&first, &different));
        assert_eq!(super::http_client_cache_key(&first_options), super::http_client_cache_key(&same_options));
        assert_ne!(super::http_client_cache_key(&first_options), super::http_client_cache_key(&different_options));
        Ok(())
    }

    #[test]
    fn incomplete_stream_finish_reason_is_rejected() {
        assert!(super::validate_stream_finish_reason(Some("stop"), "完整响应").is_ok());
        assert!(super::validate_stream_finish_reason(Some("tool_calls"), "完整响应").is_ok());
        assert!(super::validate_stream_finish_reason(None, "完整响应").is_ok());

        for reason in ["length", "content_filter", "insufficient_system_resource"] {
            let body = "前缀".repeat(1200) + "结尾标记";
            let error = super::validate_stream_finish_reason(Some(reason), &body).expect_err("incomplete stream must fail");
            let message = error.to_string();
            assert!(message.contains(reason));
            assert!(message.contains("received_chars=2404"));
            assert!(message.contains("结尾标记"));
            assert!(!message.contains(&"前缀".repeat(1200)));
        }
    }

    #[test]
    fn incomplete_non_stream_finish_reason_is_rejected() -> anyhow::Result<()> {
        let raw = r#"{"choices":[{"finish_reason":"length","message":{"content":"partial yaml"}}]}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let error = super::decode_llm_response(body, raw).expect_err("non-stream length must fail");
        let message = error.to_string();
        assert!(message.contains("finish_reason=length"));
        assert!(message.contains("received_chars="));

        let complete_raw = r#"{"choices":[{"finish_reason":"stop","message":{"content":"complete"}}]}"#;
        let (complete, _) = Dynamic::from_json(complete_raw.as_bytes())?;
        assert_eq!(super::decode_llm_response(complete, complete_raw)?.as_str(), "complete");
        Ok(())
    }

    #[test]
    fn decode_kling_async_task_id() -> anyhow::Result<()> {
        let raw = r#"{"code":0,"message":"","data":{"task_id":"task-123","task_status":"submitted"}}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let decoded = super::decode_llm_response(body, raw)?;

        assert_eq!(decoded.get_dynamic("task_id").unwrap().as_str(), "task-123");
        Ok(())
    }

    #[test]
    fn find_download_url_extracts_kling_task_result() -> anyhow::Result<()> {
        let raw = r#"{"data":{"task_status":"succeed","task_result":{"images":[{"index":0,"url":"https://example.test/kling.png"}]}}}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let url = super::find_download_url(&body).expect("url");

        assert_eq!(url.as_str(), "https://example.test/kling.png");
        Ok(())
    }

    #[test]
    fn kling_expand_task_url_uses_expand_endpoint() {
        let url = super::kling_image_expand_task_url("https://api-beijing.klingai.com", "task-123");

        assert_eq!(url, "https://api-beijing.klingai.com/v1/images/editing/expand/task-123");
    }

    #[test]
    fn kling_generation_task_url_uses_generation_endpoint() {
        let url = super::kling_image_generation_task_url("https://api-beijing.klingai.com", "task-123");

        assert_eq!(url, "https://api-beijing.klingai.com/v1/images/generations/task-123");
    }

    #[test]
    fn decode_dashscope_async_task_id() -> anyhow::Result<()> {
        let raw = r#"{"output":{"task_id":"task-123","task_status":"PENDING"},"request_id":"req"}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let decoded = super::decode_llm_response(body, raw)?;

        assert_eq!(decoded.get_dynamic("task_id").unwrap().as_str(), "task-123");
        Ok(())
    }

    #[test]
    fn find_download_url_extracts_nested_image_url() -> anyhow::Result<()> {
        let raw = r#"{"output":{"task_status":"SUCCEEDED","results":[{"url":"https://example.test/image.png"}]}}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let url = super::find_download_url(&body).expect("url");

        assert_eq!(url.as_str(), "https://example.test/image.png");
        Ok(())
    }

    #[test]
    fn find_download_url_ignores_task_id_without_url() -> anyhow::Result<()> {
        let raw = r#"{"output":{"task_id":"task-123","task_status":"RUNNING"}}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;

        assert!(super::find_download_url(&body).is_none());
        Ok(())
    }
}
