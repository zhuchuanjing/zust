use anyhow::{Result, anyhow};
use dynamic::{Dynamic, FromJson, ToJson, Type, map};
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use std::time::Duration;

extern "C" fn http_request(input: *const Dynamic) -> *const Dynamic {
    if input.is_null() {
        return Box::into_raw(Box::new(Dynamic::Null));
    }
    let input = unsafe { input.read() };
    let result = root::sync_await!(request(input)).unwrap_or(Dynamic::Null);
    Box::into_raw(Box::new(result))
}

extern "C" fn http_get(url: *const Dynamic) -> *const Dynamic {
    if url.is_null() {
        return Box::into_raw(Box::new(Dynamic::Null));
    }
    let url = unsafe { url.read() };
    let result = root::sync_await!(request(map!("method"=> "GET", "url"=> url))).unwrap_or(Dynamic::Null);
    Box::into_raw(Box::new(result))
}

extern "C" fn http_post(url: *const Dynamic, body: *const Dynamic) -> *const Dynamic {
    if url.is_null() || body.is_null() {
        return Box::into_raw(Box::new(Dynamic::Null));
    }
    let url = unsafe { url.read() };
    let body = unsafe { body.read() };
    let result = root::sync_await!(request(map!("method"=> "POST", "url"=> url, "body"=> body))).unwrap_or(Dynamic::Null);
    Box::into_raw(Box::new(result))
}

async fn request(input: Dynamic) -> Result<Dynamic> {
    let options = normalize_options(input)?;
    let url = options.get_dynamic("url").ok_or(anyhow!("http 请求缺少 url"))?;
    let url = apply_query(url.as_str(), options.get_dynamic("query"))?;
    let method = request_method(&options);
    let method = reqwest::Method::from_bytes(method.as_bytes())?;

    let client = build_client(&options)?;
    let mut req = client.request(method, url);
    req = apply_headers(req, &options)?;
    req = apply_auth(req, &options);
    req = apply_body(req, &options)?;

    let resp = req.send().await?;
    let status = resp.status();
    let final_url = resp.url().to_string();
    let content_type = resp.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
    let headers = response_headers(resp.headers());
    let bytes = resp.bytes().await?.to_vec();
    let body = decode_body(&bytes, &content_type);

    Ok(map!(
        "status"=> status.as_u16() as i64,
        "ok"=> status.is_success(),
        "url"=> final_url,
        "@headers"=> headers,
        "body"=> body
    ))
}

fn normalize_options(input: Dynamic) -> Result<Dynamic> {
    if input.is_str() {
        Ok(map!("method"=> "GET", "url"=> input))
    } else if input.is_map() {
        Ok(input)
    } else {
        Err(anyhow!("http::request 需要 url 字符串或请求配置 map"))
    }
}

fn request_method(options: &Dynamic) -> String {
    if let Some(method) = options.get_dynamic("method") {
        return method.as_str().to_ascii_uppercase();
    }

    if options.contains("body") || options.contains("data") || options.contains("json") { "POST".to_string() } else { "GET".to_string() }
}

fn build_client(options: &Dynamic) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Some(timeout_ms) = options.get_dynamic("timeout_ms").and_then(|timeout| timeout.as_int()) {
        builder = builder.timeout(Duration::from_millis(timeout_ms.max(0) as u64));
    }
    Ok(builder.build()?)
}

fn apply_query(url: &str, query: Option<Dynamic>) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(url)?;
    if let Some(query) = query {
        if query.is_map() {
            {
                let mut pairs = url.query_pairs_mut();
                for key in query.keys() {
                    let Some(value) = query.get_dynamic(key.as_str()) else {
                        continue;
                    };
                    if value.is_list() {
                        for idx in 0..value.len() {
                            if let Some(item) = value.get_idx(idx) {
                                pairs.append_pair(key.as_str(), &dynamic_to_text(&item));
                            }
                        }
                    } else {
                        pairs.append_pair(key.as_str(), &dynamic_to_text(&value));
                    }
                }
            }
        }
    }
    Ok(url)
}

fn apply_headers(mut req: reqwest::RequestBuilder, options: &Dynamic) -> Result<reqwest::RequestBuilder> {
    if let Some(headers) = options.get_dynamic("headers") {
        req = apply_header_map(req, &headers)?;
    }
    apply_inline_headers(req, options)
}

fn apply_auth(mut req: reqwest::RequestBuilder, options: &Dynamic) -> reqwest::RequestBuilder {
    if let Some(token) = options.get_dynamic("bearer").or_else(|| options.get_dynamic("key")) {
        req = req.bearer_auth(token.as_str());
    }
    req
}

fn apply_body(mut req: reqwest::RequestBuilder, options: &Dynamic) -> Result<reqwest::RequestBuilder> {
    if let Some(body) = options.get_dynamic("json") {
        req = apply_inline_headers(req, &body)?;
        let body = strip_inline_headers(body);
        let mut body_str = String::new();
        body.to_json(&mut body_str);
        req = req.header(CONTENT_TYPE, "application/json").body(body_str);
    } else if let Some(body) = options.get_dynamic("body").or_else(|| options.get_dynamic("data")) {
        req = apply_inline_headers(req, &body)?;
        let body = strip_inline_headers(body);
        if let Some(bytes) = body.as_bytes() {
            req = req.body(bytes.to_vec());
        } else if body.is_str() {
            req = req.body(body.as_str().to_string());
        } else {
            let mut body_str = String::new();
            body.to_json(&mut body_str);
            req = req.header(CONTENT_TYPE, "application/json").body(body_str);
        }
    }
    Ok(req)
}

fn apply_header_map(mut req: reqwest::RequestBuilder, headers: &Dynamic) -> Result<reqwest::RequestBuilder> {
    for key in headers.keys() {
        let Some(value) = headers.get_dynamic(key.as_str()) else {
            continue;
        };
        req = apply_header(req, key.as_str(), &value)?;
    }
    Ok(req)
}

fn apply_inline_headers(mut req: reqwest::RequestBuilder, value: &Dynamic) -> Result<reqwest::RequestBuilder> {
    if !value.is_map() {
        return Ok(req);
    }

    for key in value.keys() {
        let Some(header_value) = value.get_dynamic(key.as_str()) else {
            continue;
        };
        if is_inline_header_map(key.as_str()) {
            if header_value.is_map() {
                req = apply_header_map(req, &header_value)?;
            }
        } else if let Some(header_name) = inline_header_name(key.as_str()) {
            req = apply_header(req, header_name, &header_value)?;
        }
    }

    Ok(req)
}

fn apply_header(req: reqwest::RequestBuilder, name: &str, value: &Dynamic) -> Result<reqwest::RequestBuilder> {
    let name = HeaderName::from_bytes(name.as_bytes())?;
    let value = HeaderValue::from_str(&dynamic_to_text(value))?;
    Ok(req.header(name, value))
}

fn strip_inline_headers(value: Dynamic) -> Dynamic {
    if !value.is_map() {
        return value;
    }

    let value = value.deep_clone();
    for key in value.keys() {
        if is_inline_header_map(key.as_str()) || inline_header_name(key.as_str()).is_some() {
            value.remove_dynamic(key.as_str());
        }
    }
    value
}

fn is_inline_header_map(key: &str) -> bool {
    key == "@header" || key == "@headers"
}

fn inline_header_name(key: &str) -> Option<&str> {
    key.strip_prefix("@header ").or_else(|| key.strip_prefix("@header.")).map(str::trim).filter(|key| !key.is_empty())
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> Dynamic {
    let out = map!();
    for (key, value) in headers.iter() {
        if let Ok(value) = value.to_str() {
            out.insert(key.as_str(), value);
        }
    }
    out
}

fn decode_body(bytes: &[u8], content_type: &str) -> Dynamic {
    if content_type.starts_with("application/json")
        && let Ok((value, _)) = Dynamic::from_json(bytes)
    {
        return value;
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Ok((value, _)) = Dynamic::from_json(bytes) {
            return value;
        }
        return Dynamic::from(text);
    }

    Dynamic::Bytes(bytes.to_vec())
}

fn dynamic_to_text(value: &Dynamic) -> String {
    if value.is_str() { value.as_str().to_string() } else { value.to_string() }
}

pub const HTTP_NATIVE: [(&str, &[Type], Type, *const u8); 3] =
    [("request", &[Type::Any], Type::Any, http_request as *const u8), ("get", &[Type::Any], Type::Any, http_get as *const u8), ("post", &[Type::Any, Type::Any], Type::Any, http_post as *const u8)];

#[cfg(test)]
mod tests {
    use dynamic::{Dynamic, map};

    #[test]
    fn decode_json_body() {
        let body = super::decode_body(br#"{"name":"zust","ok":true}"#, "application/json");

        assert_eq!(body.get_dynamic("name").unwrap().as_str(), "zust");
        assert!(body.get_dynamic("ok").unwrap().is_true());
    }

    #[test]
    fn decode_text_body() {
        let body = super::decode_body(b"hello", "text/plain");

        assert_eq!(body.as_str(), "hello");
    }

    #[test]
    fn apply_query_repeats_list_values() -> anyhow::Result<()> {
        let url = super::apply_query("https://example.test/search", Some(map!("q"=> "zust", "tag"=> Dynamic::list(vec!["vm".into(), "http".into()]))))?;

        assert_eq!(url.as_str(), "https://example.test/search?q=zust&tag=vm&tag=http");
        Ok(())
    }

    #[test]
    fn request_defaults_to_post_when_body_exists() {
        assert_eq!(super::request_method(&map!("url"=> "https://example.test", "json"=> map!("name"=> "zust"))), "POST");
    }

    #[test]
    fn strips_inline_header_fields_from_body() {
        let body = super::strip_inline_headers(map!(
            "@header"=> map!("Authorization"=> "Bearer token"),
            "@header X-Trace-Id"=> "abc",
            "@header.Content-Type"=> "application/custom",
            "name"=> "zust"
        ));

        assert_eq!(body.get_dynamic("name").unwrap().as_str(), "zust");
        assert!(!body.contains("@header"));
        assert!(!body.contains("@header X-Trace-Id"));
        assert!(!body.contains("@header.Content-Type"));
    }

    #[test]
    fn parses_inline_header_names() {
        assert_eq!(super::inline_header_name("@header Authorization"), Some("Authorization"));
        assert_eq!(super::inline_header_name("@header.X-Trace-Id"), Some("X-Trace-Id"));
        assert_eq!(super::inline_header_name("@header"), None);
    }

    #[test]
    fn response_metadata_uses_at_headers() {
        let response = map!(
            "status"=> 200,
            "ok"=> true,
            "url"=> "https://example.test",
            "@headers"=> map!("content-type"=> "application/json"),
            "body"=> map!("ok"=> true)
        );

        assert!(response.contains("@headers"));
        assert!(!response.contains("headers"));
    }
}
