use anyhow::{Result, anyhow};
use dynamic::{Dynamic, FromJson, ToJson};

pub fn to_markdown(d: &Dynamic, buf: &mut String) {
    if d.is_vec() {
        //简单的 Vec<float> Vec<int> 按照 json
        d.to_json(buf);
    } else if let Dynamic::Map(m) = d {
        for (key, v) in m.read().unwrap().iter() {
            buf.push_str(&format!("#### ```{}```\n", key));
            to_markdown(v, buf);
            buf.push('\n');
        }
    } else if let Dynamic::Bytes(bytes) = d {
        buf.push_str(&format!("[{}...]", hex::encode(&bytes[..8])));
    } else {
        let len = d.len();
        if len >= 1 {
            for idx in 0..len {
                buf.push_str("- ");
                to_markdown(&d.get_idx(idx).unwrap(), buf);
                buf.push_str("\n");
            }
        } else {
            buf.push_str(&d.to_string());
        }
    }
}

use dynamic::{list, map};
use futures_util::stream::StreamExt;
use tokio::io::BufReader;
use tokio_util::io::StreamReader; // 关键转换工具

pub async fn complete(openai: Dynamic, msg: Dynamic, tx: Option<Dynamic>) -> Result<Dynamic> {
    if uses_responses_api(&openai, &msg) {
        let body = if msg.is_map() && msg.contains("input") { msg } else { map!("input"=> list!(map!("role"=> "user", "content"=> response_content(msg)))) };
        return post("responses", openai, body, tx).await;
    }

    if msg.is_list() {
        let mut list = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            list.push(chat_content_item(msg.get_idx(idx).unwrap()));
        }
        post("chat/completions", openai, map!("messages"=> list!(map!("role"=> "user", "content"=> list))), tx).await
    } else {
        let mut msg_text = String::new();
        to_markdown(&msg, &mut msg_text);
        post("chat/completions", openai, map!("messages"=> list!(map!("role"=> "user", "content"=> msg_text))), tx).await
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

fn chat_content_item(item: Dynamic) -> Dynamic {
    if item.is_map() && item.contains("type") { item } else { map!("type"=> "text", "text"=> item) }
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
    let mut msg_text = String::new();
    if msg.is_list() {
        let mut list = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            list.push(msg.get_idx(idx).unwrap());
        }
        post("images/generations", openai, map!("prompt"=> list.to_markdown()), tx).await
    } else {
        to_markdown(&msg, &mut msg_text);
        post("images/generations", openai, map!("prompt"=> msg_text), tx).await
    }
}

pub async fn tts(openai: Dynamic, msg: Dynamic) -> Result<Dynamic> {
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
    let url = bigmodel.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let app_id = bigmodel.get_dynamic("app_id").ok_or(anyhow!("没有 app_id"))?;
    let access_token = bigmodel.get_dynamic("access_token").ok_or(anyhow!("没有 access_token"))?;
    let audio = if audio.contains("url") {
        audio
    } else if audio.contains("data") {
        let audio_base64 = general_purpose::STANDARD.encode(audio.get_dynamic("data").unwrap().as_bytes().ok_or(anyhow!("没有音频数据"))?);
        map!("data"=> audio_base64)
    } else if audio.contains("audio") {
        let audio_base64 = general_purpose::STANDARD.encode(audio.get_dynamic("audio").unwrap().as_bytes().ok_or(anyhow!("没有音频数据"))?);
        map!("data"=> audio_base64)
    } else {
        return Err(anyhow!("没有 url 也没有 data"));
    };

    let body = map!("user"=> map!("uid"=> app_id.clone()), "audio"=> audio, "request"=> map!("model_name"=> "bigmodel"));

    let client = reqwest::Client::new();
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

fn copy_request_options(options: &Dynamic, msg: &Dynamic) {
    copy_request_options_except(options, msg, &[]);
}

fn copy_request_options_except(options: &Dynamic, msg: &Dynamic, skipped_keys: &[&str]) {
    for key in options.keys() {
        if key != "url" && key != "key" && key != "api" && key != "endpoint" && key != "method" && !skipped_keys.iter().any(|skipped| key.as_str() == *skipped) {
            msg.insert(key.clone(), options.get_dynamic(key.as_str()).unwrap());
        }
    }
}

pub async fn post_binary(method: &str, openai: Dynamic, msg: Dynamic) -> Result<Dynamic> {
    let url = openai.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let key = openai.get_dynamic("key");

    copy_request_options_except(&openai, &msg, &["stream"]);

    let client = reqwest::Client::new();
    let mut body_str = String::new();
    msg.to_json(&mut body_str);
    log::info!("{}", body_str);

    let resp = if let Some(key) = key {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").header("authorization", format!("Bearer {}", key.as_str())).body(body_str).send().await?
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
    //不能把 dynamic 作为耗材使用
    let url = openai.get_dynamic("url").ok_or(anyhow!("没有 url"))?;
    let key = openai.get_dynamic("key");

    let is_stream = if tx.is_none() {
        openai.insert("stream", false);
        false
    } else {
        openai.get_dynamic("stream").and_then(|is_stream| is_stream.as_bool()).unwrap_or(true)
    };

    copy_request_options(&openai, &msg);

    normalize_glm_tools(&msg);

    let client = reqwest::Client::new();
    let mut body_str = String::new();
    msg.to_json(&mut body_str);
    log::info!("{}", body_str);

    let resp = if let Some(key) = key {
        client.post(&format!("{}/{}", url.as_str(), method)).header("Content-Type", "application/json").header("authorization", format!("Bearer {}", key.as_str())).body(body_str).send().await?
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
    loop {
        line.clear(); // 清空上一行的内容
        if buf_reader.read_line(&mut line).await? == 0 {
            break;
        }
        if is_stream && line.trim().starts_with("data") {
            if let Some(start) = line.find('{') {
                if let Ok((v, _)) = Dynamic::from_json(&line.as_bytes()[start..]) {
                    if let Some(choice) = v.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|choices| choices.into_iter().next()) {
                        if let Some(content) = choice.remove_dynamic("delta").and_then(|v| v.remove_dynamic("content")) {
                            tx.as_ref().map(|tx| {
                                log::info!("{:?}", content);
                                notify(tx, map!("text"=> content))
                            });
                        }
                    }
                }
            }
        } else {
            text.push_str(&line);
        }
    }
    if is_stream {
        tx.as_ref().map(|tx| notify(tx, Dynamic::Null));
    }
    log::info!("{:#?}", &text);
    if !status.is_success() {
        return Err(anyhow!("LLM 请求失败 HTTP {}: {}", status.as_u16(), text.trim()));
    }
    let (t, _) = Dynamic::from_json(text.as_bytes()).map_err(|e| {
        tx.as_ref().map(|tx| notify(tx, Dynamic::Null));
        anyhow!("LLM 响应不是合法 JSON: {e}; body: {}", text.trim())
    })?;

    decode_llm_response(t, text.trim())
}

fn decode_llm_response(t: Dynamic, raw_text: &str) -> Result<Dynamic> {
    if let Some(data) = t.remove_dynamic("data").and_then(|c| c.into_vec::<Dynamic>()).and_then(|v| v.into_iter().next()) {
        return Ok(data);
    }
    if let Some(content) = decode_responses_text(&t) {
        return decode_text_content(content, raw_text);
    }
    let choice = t.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|v| v.into_iter().next()).ok_or_else(|| anyhow!("LLM 响应缺少 data[0] 或 choices[0]: {raw_text}"))?;
    if let Some(content) = choice.remove_dynamic("message").and_then(|m| m.remove_dynamic("content")) { decode_text_content(content, raw_text) } else { Err(anyhow!("结果不是 json")) }
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

fn decode_text_content(content: Dynamic, _raw_text: &str) -> Result<Dynamic> {
    let text = content.as_str();
    if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') {
        Dynamic::from_json(text.as_bytes()).map(|(v, _)| v)
    } else {
        let reg = regex::Regex::new(r"```(\w+)?\n([\s\S]*?)\n?```")?;
        if let Some(cap) = reg.captures_iter(text).next() {
            let lang = cap.get(1).map_or("unknown", |m| m.as_str());
            let code = cap.get(2).unwrap().as_str();
            if lang == "json" || code.trim_start().starts_with('{') || code.trim_start().starts_with('[') { Dynamic::from_json(code.as_bytes()).map(|(v, _)| v) } else { Ok(map!("lang"=> lang, "code"=> code)) }
        } else if let Some(pos) = text.find("\n{") {
            let (v, _) = Dynamic::from_json(text[pos..].as_bytes())?;
            Ok(v)
        } else {
            Ok(content)
        }
    }
}

#[cfg(test)]
mod test {
    use dynamic::{Dynamic, FromJson};
    use dynamic::{list, map};

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
    fn decode_responses_output_text_json() -> anyhow::Result<()> {
        let raw = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"{\"ok\":true,\"name\":\"pdf\"}"}]}]}"#;
        let (body, _) = Dynamic::from_json(raw.as_bytes())?;
        let decoded = super::decode_llm_response(body, raw)?;

        assert!(decoded.get_dynamic("ok").unwrap().is_true());
        assert_eq!(decoded.get_dynamic("name").unwrap().as_str(), "pdf");
        Ok(())
    }
}
