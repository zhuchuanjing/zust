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
    if msg.is_list() {
        let mut list = Dynamic::list(Vec::new());
        for idx in 0..msg.len() {
            list.push(map!("type"=> "text", "text"=> msg.get_idx(idx).unwrap()));
        }
        post("chat/completions", openai, map!("messages"=> list!(map!("role"=> "user", "content"=> list))), tx).await
    } else {
        let mut msg_text = String::new();
        to_markdown(&msg, &mut msg_text);
        post("chat/completions", openai, map!("messages"=> list!(map!("role"=> "user", "content"=> msg_text))), tx).await
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

    for key in openai.keys() {
        if key != "url" && key != "key" {
            msg.insert(key.clone(), openai.get_dynamic(key.as_str()).unwrap());
        }
    }

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
    let choice = t.remove_dynamic("choices").and_then(|c| c.into_vec::<Dynamic>()).and_then(|v| v.into_iter().next()).ok_or_else(|| anyhow!("LLM 响应缺少 data[0] 或 choices[0]: {raw_text}"))?;
    if let Some(content) = choice.remove_dynamic("message").and_then(|m| m.remove_dynamic("content")) {
        if content.as_str().trim_start().starts_with('{') {
            Dynamic::from_json(content.as_str().as_bytes()).map(|(v, _)| v)
        } else {
            let reg = regex::Regex::new(r"```(\w+)?\n([\s\S]*?)\n?```")?;
            if let Some(cap) = reg.captures_iter(content.as_str()).next() {
                let lang = cap.get(1).map_or("unknown", |m| m.as_str());
                let code = cap.get(2).unwrap().as_str();
                Ok(map!("lang"=> lang, "code"=> code))
            } else if let Some(pos) = content.as_str().find("\n{") {
                let (v, _) = Dynamic::from_json(content.as_str()[pos..].as_bytes())?;
                Ok(v)
            } else {
                Err(anyhow!("结果不是 json"))
            }
        }
    } else {
        Err(anyhow!("结果不是 json"))
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
}
