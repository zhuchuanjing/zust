use crate::memory::alloc_dynamic;
use anyhow::{Result, anyhow};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap as AxumHeaderMap, HeaderName as AxumHeaderName, HeaderValue as AxumHeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use dynamic::{Dynamic, FromJson, MsgPack, MsgUnpack, ToJson, Type, map};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{CONTENT_TYPE, HeaderName, HeaderValue};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tower_http::{cors::CorsLayer, services::ServeDir};

static WS_SENDERS: LazyLock<Mutex<BTreeMap<String, mpsc::UnboundedSender<WsCommand>>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

enum WsCommand {
    Send(Dynamic),
    Close,
}

extern "C" fn http_request(input: *const Dynamic) -> *const Dynamic {
    if input.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let input = unsafe { (&*input).clone() };
    let result = root::sync_await!(request(input)).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

extern "C" fn http_get(url: *const Dynamic) -> *const Dynamic {
    if url.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let url = unsafe { (&*url).clone() };
    let result = root::sync_await!(request(map!("method"=> "GET", "url"=> url))).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

extern "C" fn http_post(url: *const Dynamic, body: *const Dynamic) -> *const Dynamic {
    if url.is_null() || body.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let url = unsafe { (&*url).clone() };
    let body = unsafe { (&*body).clone() };
    let result = root::sync_await!(request(map!("method"=> "POST", "url"=> url, "body"=> body))).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

extern "C" fn http_serve(input: *const Dynamic) -> *const Dynamic {
    if input.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let input = unsafe { (&*input).clone() };
    alloc_dynamic(start_server(input))
}

#[cfg(feature = "llm")]
extern "C" fn http_upload(config: *const Dynamic, object_name: *const Dynamic, bytes: *const Dynamic) -> *const Dynamic {
    if config.is_null() || object_name.is_null() || bytes.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let config = unsafe { (&*config).clone() };
    let object_name = unsafe { (&*object_name).clone() };
    let bytes = unsafe { (&*bytes).clone() };
    alloc_dynamic(upload_bytes(config, object_name, bytes))
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
    // proxy 支持：options["proxy"] = "socks5://127.0.0.1:1080" 或 "http://..."。
    // reqwest 的 socks feature 已在 Cargo.toml 启用，否则 socks5:// 在编译期不存在。
    // Proxy::all 覆盖 http 和 https，对单一出口代理场景足够。
    if let Some(proxy) = options.get_dynamic("proxy").map(|v| v.as_str().to_string()) {
        if !proxy.is_empty() {
            builder = builder.proxy(reqwest::Proxy::all(&proxy)?);
        }
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

#[cfg(feature = "llm")]
fn upload_bytes(config: Dynamic, object_name: Dynamic, bytes: Dynamic) -> Dynamic {
    match upload_bytes_result(config, object_name, bytes) {
        Ok(result) => result,
        Err(err) => map!("ok"=> false, "error"=> err.to_string()),
    }
}

#[cfg(feature = "llm")]
fn upload_bytes_result(config: Dynamic, object_name: Dynamic, bytes: Dynamic) -> Result<Dynamic> {
    let object_name = object_name.as_str().to_string();
    if object_name.trim().is_empty() {
        return Err(anyhow!("http::upload object_name missing"));
    }
    let bytes = bytes.as_bytes().ok_or_else(|| anyhow!("http::upload expects bytes"))?.to_vec();
    let upload_name = object_name.clone();
    let link_config = config.deep_clone();
    let oss_url = root::sync_await!(async move { llm::oss::upload(config, &upload_name, bytes).await })?;
    let url = llm::oss::get_link(link_config, &oss_url, 600)?;
    Ok(map!("ok"=> true, "object_name"=> object_name, "oss_url"=> oss_url, "url"=> url))
}

fn start_server(input: Dynamic) -> Dynamic {
    match server_options(input).and_then(|options| {
        prepare_server_runtime(&options)?;
        let info = map!("host"=> options.host.clone(), "port"=> options.port as i64, "api_prefix"=> options.api_prefix.clone());
        if !options.upload_paths.is_empty() {
            info.insert("upload", Dynamic::list(options.upload_paths.iter().cloned().map(Dynamic::from).collect()));
        }
        if !options.ws_paths.is_empty() {
            info.insert("ws", Dynamic::list(options.ws_paths.iter().cloned().map(Dynamic::from).collect()));
        }
        let task_options = options.clone();
        let id = root::start_task(info, move || {
            Box::pin(async move {
                run_server(task_options).await?;
                Ok(())
            })
        });
        Ok(map!("ok"=> true, "id"=> id, "url"=> format!("http://{}:{}", options.host, options.port)))
    }) {
        Ok(result) => result,
        Err(err) => map!("ok"=> false, "error"=> err.to_string()),
    }
}

fn prepare_server_runtime(options: &ServerOptions) -> Result<()> {
    if !options.ws_paths.is_empty() {
        ensure_ws_list()?;
    }
    Ok(())
}

#[derive(Clone)]
struct ServerOptions {
    host: String,
    port: u16,
    api_prefix: String,
    body_limit: usize,
    ws_paths: Vec<String>,
    upload_paths: Vec<String>,
    static_dirs: Vec<StaticDir>,
}

#[derive(Clone)]
struct StaticDir {
    path: String,
    dir: String,
}

fn server_options(input: Dynamic) -> Result<ServerOptions> {
    let config = if input.is_str() { map!("host"=> input) } else { input };
    if !config.is_map() {
        return Err(anyhow!("http::serve 需要配置 map"));
    }
    let host_value = config.get_dynamic("host").or_else(|| config.get_dynamic("addr")).ok_or_else(|| anyhow!("http::serve 需要 host"))?;
    let (host, port) = parse_addr(host_value.as_str())?;
    let ws_paths = config.get_dynamic("ws").map(|value| path_options(&value, Some("/ws"))).transpose()?.unwrap_or_default();
    let upload_paths = config.get_dynamic("upload").map(|value| path_options(&value, Some("/upload"))).transpose()?.unwrap_or_default();
    let static_dirs = config.get_dynamic("static").map(|value| static_options(&value)).transpose()?.unwrap_or_default();
    Ok(ServerOptions { host, port, api_prefix: "api".into(), body_limit: 20 * 1024 * 1024, ws_paths, upload_paths, static_dirs })
}

fn parse_addr(addr: &str) -> Result<(String, u16)> {
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| anyhow!("http::serve 地址需要 host:port"))?;
    let port = port.parse::<u16>().map_err(|_| anyhow!("http::serve port 非法"))?;
    let host = if host.is_empty() { "0.0.0.0" } else { host };
    Ok((host.to_string(), port))
}

async fn run_server(options: ServerOptions) -> Result<()> {
    let route = format!("/{}/{{*path}}", options.api_prefix.trim_matches('/'));
    let mut app = Router::new().route("/health", get(http_health)).route(&route, get(api_dispatch).post(api_dispatch));

    for ws_path in &options.ws_paths {
        app = app.route(ws_path, get(ws_upgrade));
    }
    for upload_path in &options.upload_paths {
        app = app.route(upload_path, post(upload_dispatch));
    }
    for static_dir in &options.static_dirs {
        let service = ServeDir::new(&static_dir.dir).append_index_html_on_directories(true);
        if static_dir.path == "/" {
            app = app.fallback_service(service);
        } else {
            app = app.nest_service(&static_dir.path, service);
        }
    }
    let app = app.layer(CorsLayer::very_permissive()).with_state(options.clone());

    let listener = tokio::net::TcpListener::bind((options.host.as_str(), options.port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn path_options(value: &Dynamic, true_path: Option<&str>) -> Result<Vec<String>> {
    if value.is_list() {
        let mut paths = Vec::new();
        for idx in 0..value.len() {
            if let Some(item) = value.get_idx(idx) {
                paths.extend(path_options(&item, true_path)?);
            }
        }
        return Ok(paths);
    }

    Ok(path_option(value, true_path).into_iter().collect())
}

fn path_option(value: &Dynamic, true_path: Option<&str>) -> Option<String> {
    if value.as_bool() == Some(true) {
        return true_path.map(str::to_string);
    }
    let path = value.as_str().trim().trim_matches('/').to_string();
    if path.is_empty() { None } else { Some(format!("/{path}")) }
}

fn static_options(value: &Dynamic) -> Result<Vec<StaticDir>> {
    if value.is_str() {
        let dir = value.as_str().trim();
        if dir.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![StaticDir { path: "/".into(), dir: dir.into() }]);
    }

    if value.is_list() {
        let mut dirs = Vec::new();
        for idx in 0..value.len() {
            if let Some(item) = value.get_idx(idx) {
                dirs.extend(static_options(&item)?);
            }
        }
        return Ok(dirs);
    }

    if value.is_map() {
        let dir = value.get_dynamic("dir").or_else(|| value.get_dynamic("root")).or_else(|| value.get_dynamic("directory")).ok_or_else(|| anyhow!("http static 需要 dir"))?.as_str().trim().to_string();
        if dir.is_empty() {
            return Ok(Vec::new());
        }
        let path = value.get_dynamic("path").or_else(|| value.get_dynamic("url")).map(|path| public_path(path.as_str())).unwrap_or_else(|| static_mount_path(&dir));
        return Ok(vec![StaticDir { path, dir }]);
    }

    Err(anyhow!("http static 需要字符串、map 或 list"))
}

fn static_mount_path(dir: &str) -> String {
    let name = Path::new(dir).file_name().and_then(|name| name.to_str()).filter(|name| !name.is_empty()).unwrap_or("static");
    public_path(name)
}

fn public_path(path: &str) -> String {
    let path = path.trim().trim_matches('/');
    if path.is_empty() { "/".into() } else { format!("/{path}") }
}

async fn http_health() -> Response {
    json_response(StatusCode::OK, map!("ok"=> true, "service"=> "zust-http"))
}

async fn api_dispatch(axum::extract::State(options): axum::extract::State<ServerOptions>, req: Request<Body>) -> Response {
    let route_match = api_route_match(&options, &req);
    let route = route_match.route.clone();
    match api_payload(req, options.body_limit).await.and_then(|payload| {
        apply_path_params(&payload, route_match.params);
        root::send_msg(&route, payload).map_err(|err| anyhow!("dispatch {route}: {err}"))
    }) {
        Ok(result) => to_response(result).unwrap_or_else(|err| json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

struct RouteMatch {
    route: String,
    params: Dynamic,
}

fn api_route_match(options: &ServerOptions, req: &Request<Body>) -> RouteMatch {
    let method = req.method().as_str().to_ascii_lowercase();
    let prefix = format!("/{}/", options.api_prefix.trim_matches('/'));
    let path = req.uri().path().strip_prefix(&prefix).unwrap_or("").trim_matches('/');
    let exact = format!("local/http/{method}/{path}");
    if root::contains(&exact) {
        return RouteMatch { route: exact, params: map!() };
    }
    if let Some((route, params)) = dynamic_api_route(&method, path) {
        return RouteMatch { route, params };
    }
    RouteMatch { route: exact, params: map!() }
}

fn dynamic_api_route(method: &str, path: &str) -> Option<(String, Dynamic)> {
    let root_path = format!("local/http/{method}");
    let mut routes = Vec::new();
    collect_routes(&root_path, &mut routes, 0);
    let request_segments = path_segments(path);
    let mut best: Option<(String, Dynamic, usize)> = None;
    for route in routes {
        let Some(pattern) = route.strip_prefix(&(root_path.clone() + "/")) else {
            continue;
        };
        if let Some((params, score)) = match_route_pattern(pattern, &request_segments) {
            if best.as_ref().map(|(_, _, best_score)| score > *best_score).unwrap_or(true) {
                best = Some((route, params, score));
            }
        }
    }
    best.map(|(route, params, _)| (route, params))
}

fn collect_routes(prefix: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 32 {
        return;
    }
    if root::contains(prefix) {
        out.push(prefix.to_string());
    }
    let Ok(children) = root::dir(prefix, false) else {
        return;
    };
    if !children.is_list() {
        return;
    }
    for idx in 0..children.len() {
        let Some(child) = children.get_idx(idx) else {
            continue;
        };
        let child = child.as_str();
        let child_path = if child.starts_with(prefix) {
            child.to_string()
        } else if let Some(local_child) = child.strip_prefix("http/") {
            format!("local/http/{}", local_child.trim_matches('/'))
        } else {
            format!("{}/{}", prefix.trim_matches('/'), child.trim_matches('/'))
        };
        collect_routes(&child_path, out, depth + 1);
    }
}

fn match_route_pattern(pattern: &str, request_segments: &[&str]) -> Option<(Dynamic, usize)> {
    let pattern_segments = path_segments(pattern);
    if pattern_segments.len() != request_segments.len() {
        return None;
    }
    let params = map!();
    let mut score = 0usize;
    for (pattern, actual) in pattern_segments.iter().zip(request_segments.iter()) {
        if let Some(name) = pattern.strip_prefix(':') {
            if name.is_empty() || actual.is_empty() {
                return None;
            }
            params.insert(name, percent_decode(actual));
        } else if *pattern == *actual {
            score += 1;
        } else {
            return None;
        }
    }
    Some((params, score))
}

fn path_segments(path: &str) -> Vec<&str> {
    path.trim_matches('/').split('/').filter(|part| !part.is_empty()).collect()
}

fn apply_path_params(payload: &Dynamic, params: Dynamic) {
    if !params.is_map() {
        return;
    }
    payload.insert("@path_params", params.clone());
    for key in params.keys() {
        if !payload.contains(key.as_str()) {
            if let Some(value) = params.get_dynamic(key.as_str()) {
                payload.insert(key, value);
            }
        }
    }
}

async fn upload_dispatch(axum::extract::State(options): axum::extract::State<ServerOptions>, req: Request<Body>) -> Response {
    match raw_payload(req, options.body_limit).await.and_then(|payload| {
        let parsed = parse_multipart_result(&payload)?;
        payload.append(parsed);
        root::send_msg("local/http/upload", payload).map_err(|err| anyhow!("dispatch local/http/upload: {err}"))
    }) {
        Ok(result) => to_response(result).unwrap_or_else(|err| json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())),
        Err(err) => json_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

async fn api_payload(req: Request<Body>, body_limit: usize) -> Result<Dynamic> {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_ascii_lowercase();
    let path = parts.uri.path().trim_start_matches('/').to_string();
    let payload = map!("@method"=> method, "@path"=> path, "@header"=> headers_to_dynamic(&parts.headers));
    if let Some(query) = parts.uri.query() {
        payload.insert("@query", query_to_dynamic(query));
    }
    let body = to_bytes(body, body_limit).await?;
    if !body.is_empty() {
        match Dynamic::from_json(&body) {
            Ok((json, _)) => {
                payload.append(json);
                payload.insert("@raw_body", body_to_dynamic(body));
            }
            Err(err) => {
                payload.insert("@body_error", err.to_string());
                let raw_body = body_to_dynamic(body);
                payload.insert("@body", raw_body.clone());
                payload.insert("@raw_body", raw_body);
            }
        }
    }
    Ok(payload)
}

async fn raw_payload(req: Request<Body>, body_limit: usize) -> Result<Dynamic> {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_ascii_lowercase();
    let path = parts.uri.path().trim_start_matches('/').to_string();
    let payload = map!("@method"=> method, "@path"=> path, "@header"=> headers_to_dynamic(&parts.headers));
    if let Some(query) = parts.uri.query() {
        payload.insert("@query", query_to_dynamic(query));
    }
    let body = to_bytes(body, body_limit).await?;
    payload.insert("@body", Dynamic::Bytes(body.to_vec()));
    Ok(payload)
}

fn parse_multipart_dynamic(input: Dynamic) -> Dynamic {
    match parse_multipart_result(&input) {
        Ok(result) => result,
        Err(err) => map!("ok"=> false, "error"=> err.to_string()),
    }
}

fn parse_multipart_result(input: &Dynamic) -> Result<Dynamic> {
    let headers = input.get_dynamic("@header").or_else(|| input.get_dynamic("@headers")).ok_or_else(|| anyhow!("multipart headers missing"))?;
    let content_type = header_value(&headers, "content-type").ok_or_else(|| anyhow!("multipart content-type missing"))?;
    let boundary = multipart_boundary(&content_type).ok_or_else(|| anyhow!("multipart boundary missing"))?;
    let body = input.get_dynamic("@body").ok_or_else(|| anyhow!("multipart body missing"))?;
    let body = if let Some(bytes) = body.as_bytes() { bytes.to_vec() } else { body.as_str().as_bytes().to_vec() };
    parse_multipart_bytes(&body, &boundary)
}

fn header_value(headers: &Dynamic, key: &str) -> Option<String> {
    if !headers.is_map() {
        return None;
    }
    for name in headers.keys() {
        if name.eq_ignore_ascii_case(key) {
            return headers.get_dynamic(name.as_str()).map(|value| value.as_str().to_string());
        }
    }
    None
}

fn multipart_boundary(content_type: &str) -> Option<String> {
    content_type.split(';').map(str::trim).find_map(|part| {
        let value = part.strip_prefix("boundary=")?;
        Some(value.trim_matches('"').to_string())
    })
}

fn parse_multipart_bytes(body: &[u8], boundary: &str) -> Result<Dynamic> {
    let marker = format!("--{boundary}").into_bytes();
    let mut pos = find_bytes(body, &marker).ok_or_else(|| anyhow!("multipart boundary not found"))? + marker.len();
    let data = map!();

    loop {
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        if body.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        } else if body.get(pos..pos + 1) == Some(b"\n") {
            pos += 1;
        }

        let (headers_end, sep_len) = find_header_end(&body[pos..]).ok_or_else(|| anyhow!("multipart part headers missing"))?;
        let header_bytes = &body[pos..pos + headers_end];
        let data_start = pos + headers_end + sep_len;
        let (data_end, next_pos) = find_next_part(body, data_start, &marker).ok_or_else(|| anyhow!("multipart next boundary missing"))?;
        let part_body = &body[data_start..data_end];
        let headers = parse_part_headers(header_bytes);
        let disposition = header_value(&headers, "content-disposition").unwrap_or_default();
        let name = disposition_param(&disposition, "name").unwrap_or_default();

        if !name.is_empty() {
            insert_multi(&data, &name, Dynamic::Bytes(part_body.to_vec()));
        }
        pos = next_pos;
    }

    Ok(data)
}

fn find_header_end(bytes: &[u8]) -> Option<(usize, usize)> {
    find_bytes(bytes, b"\r\n\r\n").map(|idx| (idx, 4)).or_else(|| find_bytes(bytes, b"\n\n").map(|idx| (idx, 2)))
}

fn find_next_part(body: &[u8], start: usize, marker: &[u8]) -> Option<(usize, usize)> {
    if let Some(idx) = find_bytes(&body[start..], &[b"\r\n", marker].concat()) {
        let boundary_start = start + idx;
        return Some((boundary_start, boundary_start + 2 + marker.len()));
    }
    if let Some(idx) = find_bytes(&body[start..], &[b"\n", marker].concat()) {
        let boundary_start = start + idx;
        return Some((boundary_start, boundary_start + 1 + marker.len()));
    }
    None
}

fn find_bytes(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > bytes.len() {
        return None;
    }
    bytes.windows(needle.len()).position(|window| window == needle)
}

fn parse_part_headers(bytes: &[u8]) -> Dynamic {
    let headers = map!();
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    headers
}

fn disposition_param(disposition: &str, key: &str) -> Option<String> {
    for part in disposition.split(';').map(str::trim) {
        if let Some((name, value)) = part.split_once('=')
            && name == key
        {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

fn insert_multi(target: &Dynamic, key: &str, value: Dynamic) {
    if key.is_empty() {
        return;
    }
    if let Some(existing) = target.get_dynamic(key) {
        if existing.is_list() {
            let mut existing = existing;
            existing.push(value);
        } else {
            target.insert(key, Dynamic::list(vec![existing, value]));
        }
    } else {
        target.insert(key, value);
    }
}

async fn ws_upgrade(headers: AxumHeaderMap, ws: WebSocketUpgrade, axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>) -> Response {
    let token = bearer_token(&headers).or_else(|| query.get("token").cloned()).unwrap_or_default();
    let query = map_from_string_map(query);
    let auth_payload = map!("token"=> token.clone(), "@header"=> headers_to_dynamic(&headers), "@query"=> query.clone());
    let session = match optional_dispatch("local/ws_handlers/auth", auth_payload) {
        Ok(Some(auth)) => {
            if auth.get_dynamic("ok").and_then(|value| value.as_bool()) == Some(false) {
                let error = auth.get_dynamic("error").map(|value| value.as_str().to_string()).unwrap_or_else(|| "invalid websocket auth".into());
                return json_error(StatusCode::UNAUTHORIZED, error);
            }
            auth.get_dynamic("data").unwrap_or(auth)
        }
        Ok(None) => Dynamic::Null,
        Err(err) => return json_error(StatusCode::UNAUTHORIZED, err.to_string()),
    };
    ws.on_upgrade(move |socket| handle_socket(socket, token, session))
}

async fn handle_socket(socket: WebSocket, token: String, session: Dynamic) {
    let idx = match register_ws_sender() {
        Ok(idx) => idx,
        Err(err) => {
            log::warn!("websocket sender register failed: {err}");
            return;
        }
    };
    let idx_key = idx.to_string();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<WsCommand>();
    {
        let mut senders = WS_SENDERS.lock().expect("ws sender registry poisoned");
        senders.insert(idx_key.clone(), out_tx);
    }

    let (mut sender, mut receiver) = socket.split();
    let writer_idx = idx_key.clone();
    let writer = tokio::spawn(async move {
        while let Some(command) = out_rx.recv().await {
            let close = matches!(command, WsCommand::Close);
            let result = match command {
                WsCommand::Send(payload) => sender.send(Message::Binary(dynamic_msgpack(payload).into())).await,
                WsCommand::Close => sender.send(Message::Close(None)).await,
            };
            if let Err(err) = result {
                log::warn!("websocket send failed idx={writer_idx}: {err}");
                break;
            }
            if close {
                break;
            }
        }
    });

    let _ = optional_dispatch("local/ws_handlers/connect", map!("idx"=> idx as i64, "token"=> token.clone(), "session"=> session.clone()));

    while let Some(message) = receiver.next().await {
        let payload = match message {
            Ok(Message::Binary(bytes)) => ws_binary_payload(idx, &token, &session, &bytes),
            Ok(Message::Text(text)) => ws_text_payload(idx, &token, &session, &text),
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Err(err) => {
                log::warn!("websocket receive failed idx={idx}: {err}");
                break;
            }
        };
        if let Err(err) = payload.and_then(|payload| optional_dispatch("local/ws_handlers/message", payload).map(|_| ())) {
            let error_message = map!("ok"=> false, "error"=> err.to_string(), "idx"=> idx as i64, "id"=> 500);
            let _ = root::send_idx_msg("local/ws", idx, error_message);
        }
    }

    let _ = optional_dispatch("local/ws_handlers/disconnect", map!("idx"=> idx as i64, "token"=> token, "session"=> session));
    {
        let mut senders = WS_SENDERS.lock().expect("ws sender registry poisoned");
        senders.remove(&idx_key);
    }
    // 回收 local/ws 列表槽位(sparse slot 设计:remove_idx 不 pack,索引稳定),
    // 避免长连接服务下 local/ws 列表随连接数单调增长泄漏内存。
    let _ = root::remove_idx("local/ws", idx);
    writer.abort();
}

fn register_ws_sender() -> Result<usize> {
    ensure_ws_list()?;
    let (mount, name) = root::get_mount("local/ws")?;
    mount.push(name, root::Object::Native(root::NativeHandler::from_fn(ws_send_native)))
}

fn ensure_ws_list() -> Result<()> {
    if root::get_mount("local/ws").and_then(|(mount, name)| mount.len(name).map(|_| ())).is_ok() {
        return Ok(());
    }
    root::add_list("local/ws")
}

fn ws_send_native(payload: Dynamic) -> Dynamic {
    let Some(idx_value) = payload.get_dynamic("idx") else {
        return map!("ok"=> false, "error"=> "missing ws idx");
    };
    let idx = idx_value.as_int().map(|value| value.to_string()).unwrap_or_else(|| idx_value.as_str().to_string());
    let close = payload.get_dynamic("@close").and_then(|value| value.as_bool()).unwrap_or(false);
    let sent = {
        let senders = WS_SENDERS.lock().expect("ws sender registry poisoned");
        senders.get(&idx).map(|tx| if close { tx.send(WsCommand::Close).is_ok() } else { tx.send(WsCommand::Send(payload.clone())).is_ok() }).unwrap_or(false)
    };
    if sent { map!("ok"=> true, "idx"=> idx) } else { map!("ok"=> false, "idx"=> idx, "error"=> "websocket sender missing") }
}

fn ws_binary_payload(idx: usize, token: &str, session: &Dynamic, bytes: &[u8]) -> Result<Dynamic> {
    let (message, _) = Dynamic::decode(bytes).map_err(|err| anyhow!("invalid msgpack websocket payload: {err}"))?;
    Ok(map!("idx"=> idx as i64, "token"=> token, "session"=> session.clone(), "message"=> message))
}

fn ws_text_payload(idx: usize, token: &str, session: &Dynamic, text: &str) -> Result<Dynamic> {
    let message = if let Ok((json, _)) = Dynamic::from_json(text.as_bytes()) { json } else { Dynamic::from(text) };
    Ok(map!("idx"=> idx as i64, "token"=> token, "session"=> session.clone(), "message"=> message))
}

fn optional_dispatch(route: &str, payload: Dynamic) -> Result<Option<Dynamic>> {
    if root::contains(route) { root::send_msg(route, payload).map(Some) } else { Ok(None) }
}

fn bearer_token(headers: &AxumHeaderMap) -> Option<String> {
    let auth = headers.get("authorization").and_then(|value| value.to_str().ok()).or_else(|| headers.get("sec-websocket-protocol").and_then(|value| value.to_str().ok()))?.trim();
    auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")).or_else(|| auth.strip_prefix("Bearer.")).or_else(|| auth.strip_prefix("bearer.")).map(str::to_string)
}

fn dynamic_msgpack(obj: Dynamic) -> Vec<u8> {
    let mut body = Vec::new();
    obj.encode(&mut body);
    body
}

fn to_response(obj: Dynamic) -> Result<Response> {
    let status = obj.get_dynamic("@status").and_then(|value| value.as_int()).and_then(|code| StatusCode::from_u16(code as u16).ok()).unwrap_or(StatusCode::OK);
    let mut builder = Response::builder().status(status);
    let headers = builder.headers_mut().ok_or_else(|| anyhow!("response builder has no headers"))?;
    if let Some(content_type) = obj.get_dynamic("@content-type") {
        headers.insert(AxumHeaderName::from_static("content-type"), AxumHeaderValue::from_str(content_type.as_str())?);
    } else {
        headers.insert(AxumHeaderName::from_static("content-type"), AxumHeaderValue::from_static("application/json;charset=utf-8"));
    }
    if let Some(body) = obj.get_dynamic("@body") {
        if let Some(bytes) = body.as_bytes() {
            return Ok(builder.body(Body::from(bytes.to_vec()))?);
        }
        return Ok(builder.body(Body::from(body.as_str().to_string()))?);
    }
    Ok(builder.body(Body::from(dynamic_json(obj)))?)
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(status, map!("ok"=> false, "error"=> message.into()))
}

fn json_response(status: StatusCode, obj: Dynamic) -> Response {
    (status, [(AxumHeaderName::from_static("content-type"), AxumHeaderValue::from_static("application/json;charset=utf-8"))], dynamic_json(obj)).into_response()
}

fn dynamic_json(obj: Dynamic) -> String {
    let mut body = String::new();
    obj.to_json(&mut body);
    body
}

fn headers_to_dynamic(headers: &AxumHeaderMap) -> Dynamic {
    let out = map!();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            out.insert(name.as_str(), value);
        }
    }
    out
}

fn query_to_dynamic(query: &str) -> Dynamic {
    let out = map!();
    for item in query.split('&').filter(|item| !item.is_empty()) {
        let (key, value) = item.split_once('=').unwrap_or((item, ""));
        out.insert(percent_decode(key), percent_decode(value));
    }
    out
}

fn map_from_string_map(source: BTreeMap<String, String>) -> Dynamic {
    let out = map!();
    for (key, value) in source {
        out.insert(key, value);
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'+' {
            out.push(b' ');
            idx += 1;
        } else if bytes[idx] == b'%' && idx + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[idx + 1]), hex_value(bytes[idx + 2])) {
                out.push((hi << 4) | lo);
                idx += 3;
            } else {
                out.push(bytes[idx]);
                idx += 1;
            }
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn body_to_dynamic(body: Bytes) -> Dynamic {
    std::str::from_utf8(&body).map(Dynamic::from).unwrap_or_else(|_| Dynamic::Bytes(body.to_vec()))
}

pub const HTTP_NATIVE: &[(&str, &[Type], Type, *const u8)] = &[
    ("request", &[Type::Any], Type::Any, http_request as *const u8),
    ("get", &[Type::Any], Type::Any, http_get as *const u8),
    ("post", &[Type::Any, Type::Any], Type::Any, http_post as *const u8),
    #[cfg(feature = "llm")]
    ("upload", &[Type::Any, Type::Any, Type::Any], Type::Any, http_upload as *const u8),
    ("serve", &[Type::Any], Type::Any, http_serve as *const u8),
];

pub fn add_root_handlers() -> Result<()> {
    root::add("local/http/parse-multipart", root::Object::Native(root::NativeHandler::from_fn(parse_multipart_dynamic)))?;
    Ok(())
}

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

    #[test]
    fn parses_server_address() -> anyhow::Result<()> {
        let (host, port) = super::parse_addr("0.0.0.0:8080")?;

        assert_eq!(host, "0.0.0.0");
        assert_eq!(port, 8080);
        Ok(())
    }

    #[test]
    fn parses_server_dynamic_config() -> anyhow::Result<()> {
        let options = super::server_options(map!(
            "host"=> "0.0.0.0:8080",
            "ws"=> Dynamic::list(vec!["/ws".into(), "/socket".into()]),
            "upload"=> Dynamic::list(vec!["/upload".into(), "/file".into()]),
            "static"=> Dynamic::list(vec![Dynamic::from("public"), map!("path"=> "/assets", "dir"=> "assets")])
        ))?;

        assert_eq!(options.ws_paths, vec!["/ws".to_string(), "/socket".to_string()]);
        assert_eq!(options.upload_paths, vec!["/upload".to_string(), "/file".to_string()]);
        assert_eq!(options.static_dirs.len(), 2);
        assert_eq!(options.static_dirs[0].dir, "public");
        assert_eq!(options.static_dirs[1].path, "/assets");
        Ok(())
    }

    #[test]
    fn prepare_server_runtime_creates_ws_list() -> anyhow::Result<()> {
        let options = super::server_options(map!("host"=> "0.0.0.0:8080", "ws"=> "/ws"))?;

        super::prepare_server_runtime(&options)?;

        let (mount, name) = root::get_mount("local/ws")?;
        assert_eq!(mount.len(name)?, 0);
        Ok(())
    }

    #[test]
    fn parses_multipart_into_name_bytes() -> anyhow::Result<()> {
        let body = b"--z\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n--z\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n\x00\xffbin\r\n--z--\r\n";
        let parsed = super::parse_multipart_bytes(body, "z")?;

        assert_eq!(parsed.get_dynamic("title").unwrap().as_bytes().unwrap(), b"hello");
        assert_eq!(parsed.get_dynamic("file").unwrap().as_bytes().unwrap(), b"\x00\xffbin");
        Ok(())
    }
}
