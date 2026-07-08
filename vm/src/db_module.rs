use crate::memory::alloc_dynamic;
use anyhow::{Result, anyhow};
use dynamic::{Dynamic, Type};
use smol_str::SmolStr;
use sqlx_core::{
    any::{Any, AnyArguments, AnyPool, AnyPoolOptions, AnyRow, AnyTypeInfoKind, driver::install_drivers},
    column::Column,
    executor::Executor,
    query::{Query, query},
    row::Row,
    value::ValueRef,
};
use std::{
    collections::BTreeMap,
    collections::HashMap,
    sync::{LazyLock, Once},
};

#[derive(Clone)]
struct PoolEntry {
    url: String,
    pool: AnyPool,
}

#[derive(Debug, Clone)]
struct DbTarget {
    pool_path: String,
    url: String,
    table: Option<String>,
    max_connections: u32,
}

#[derive(Debug, Clone)]
struct ColumnDef {
    name: String,
    ty: String,
}

#[derive(Debug, Clone)]
struct IndexDef {
    name: Option<String>,
    columns: Vec<String>,
    unique: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbKind {
    Postgres,
    MySql,
}

static POOLS: LazyLock<parking_lot::Mutex<HashMap<String, PoolEntry>>> = LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));
static INSTALL_DRIVERS: Once = Once::new();

extern "C" fn db_create(path: *const Dynamic, fields: *const Dynamic) -> bool {
    if path.is_null() || fields.is_null() {
        return false;
    }
    let path = unsafe { (&*path).clone() };
    let fields = unsafe { (&*fields).clone() };
    match root::sync_await!(create_table(path, fields)) {
        Ok(ok) => ok,
        Err(err) => {
            log::error!("db::create failed: {err:?}");
            false
        }
    }
}

extern "C" fn db_drop(path: *const Dynamic) -> bool {
    if path.is_null() {
        return false;
    }
    let path = unsafe { (&*path).clone() };
    match root::sync_await!(drop_table(path)) {
        Ok(ok) => ok,
        Err(err) => {
            log::error!("db::drop failed: {err:?}");
            false
        }
    }
}

extern "C" fn db_select(path: *const Dynamic, sql: *const Dynamic, data: *const Dynamic) -> *const Dynamic {
    if path.is_null() || sql.is_null() || data.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let path = unsafe { (&*path).clone() };
    let sql = unsafe { (&*sql).clone() };
    let data = unsafe { (&*data).clone() };
    let result = root::sync_await!(select_rows(path, sql, data)).unwrap_or_else(|err| {
        log::error!("db::select failed: {err:?}");
        Dynamic::Null
    });
    alloc_dynamic(result)
}

extern "C" fn db_exec(path: *const Dynamic, sql: *const Dynamic, data: *const Dynamic) -> i64 {
    if path.is_null() || sql.is_null() || data.is_null() {
        return -1;
    }
    let path = unsafe { (&*path).clone() };
    let sql = unsafe { (&*sql).clone() };
    let data = unsafe { (&*data).clone() };
    match root::sync_await!(exec_sql(path, sql, data)) {
        Ok(rows) => rows,
        Err(err) => {
            log::error!("db::exec failed: {err:?}");
            -1
        }
    }
}

extern "C" fn db_transaction(path: *const Dynamic, steps: *const Dynamic) -> i64 {
    if path.is_null() || steps.is_null() {
        return -1;
    }
    let path = unsafe { (&*path).clone() };
    let steps = unsafe { (&*steps).clone() };
    match root::sync_await!(transaction_sql(path, steps)) {
        Ok(rows) => rows,
        Err(err) => {
            log::error!("db::transaction failed: {err:?}");
            -1
        }
    }
}

async fn create_table(path: Dynamic, fields: Dynamic) -> Result<bool> {
    let target = resolve_target(path.as_str())?;
    let columns = parse_columns(&fields)?;
    if columns.is_empty() {
        return Err(anyhow!("db::create 至少需要一个字段"));
    }

    let table = target.table.as_deref().ok_or_else(|| anyhow!("db::create 需要表路径，例如 local/db/user；完整路径是连接配置时不会推断表名"))?;
    let kind = db_kind(&target.url)?;
    let sql = build_create_sql(table, &columns, kind)?;
    let pool = pool_for(&target).await?;
    pool.execute(sql.as_str()).await?;
    for index in parse_indexes(&fields)? {
        let sql = build_create_index_sql(table, &index, kind)?;
        pool.execute(sql.as_str()).await?;
    }
    Ok(true)
}

async fn drop_table(path: Dynamic) -> Result<bool> {
    let target = resolve_target(path.as_str())?;
    let table = target.table.as_deref().ok_or_else(|| anyhow!("db::drop 需要表路径，例如 local/db/user；完整路径是连接配置时不会推断表名"))?;
    let kind = db_kind(&target.url)?;
    let table = quote_ident(table, kind)?;
    let sql = format!("DROP TABLE IF EXISTS {table}");
    let pool = pool_for(&target).await?;
    pool.execute(sql.as_str()).await?;
    Ok(true)
}

async fn select_rows(path: Dynamic, sql: Dynamic, data: Dynamic) -> Result<Dynamic> {
    let target = resolve_target(path.as_str())?;
    let kind = db_kind(&target.url)?;
    let (sql, values) = prepare_sql(sql.as_str(), data, kind)?;
    let pool = pool_for(&target).await?;
    let rows = bind_values(query(&sql), values)?.fetch_all(&pool).await?;
    Ok(Dynamic::list(rows.into_iter().map(row_to_dynamic).collect()))
}

async fn exec_sql(path: Dynamic, sql: Dynamic, data: Dynamic) -> Result<i64> {
    let target = resolve_target(path.as_str())?;
    let kind = db_kind(&target.url)?;
    let (sql, values) = prepare_sql(sql.as_str(), data, kind)?;
    let pool = pool_for(&target).await?;
    let result = bind_values(query(&sql), values)?.execute(&pool).await?;
    Ok(result.rows_affected().min(i64::MAX as u64) as i64)
}

async fn transaction_sql(path: Dynamic, steps: Dynamic) -> Result<i64> {
    let target = resolve_target(path.as_str())?;
    let kind = db_kind(&target.url)?;
    let steps = parse_transaction_steps(&steps)?;
    let pool = pool_for(&target).await?;
    let mut tx = pool.begin().await?;
    let mut rows_affected = 0u64;

    for (sql, data) in steps {
        let (sql, values) = prepare_sql(&sql, data, kind)?;
        let result = bind_values(query(&sql), values)?.execute(&mut *tx).await?;
        rows_affected = rows_affected.saturating_add(result.rows_affected());
    }

    tx.commit().await?;
    Ok(rows_affected.min(i64::MAX as u64) as i64)
}

async fn pool_for(target: &DbTarget) -> Result<AnyPool> {
    INSTALL_DRIVERS.call_once(|| {
        install_drivers(&[sqlx_mysql::any::DRIVER, sqlx_postgres::any::DRIVER]).expect("SQLx Any drivers already installed");
    });

    if let Some(pool) = POOLS.lock().get(&target.pool_path).filter(|entry| entry.url == target.url).map(|entry| entry.pool.clone()) {
        return Ok(pool);
    }

    let pool = AnyPoolOptions::new().max_connections(target.max_connections).connect(&target.url).await?;
    POOLS.lock().insert(target.pool_path.clone(), PoolEntry { url: target.url.clone(), pool: pool.clone() });
    Ok(pool)
}

fn db_kind(url: &str) -> Result<DbKind> {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        Ok(DbKind::Postgres)
    } else if url.starts_with("mysql://") || url.starts_with("mariadb://") {
        Ok(DbKind::MySql)
    } else {
        Err(anyhow!("不支持的数据库 URL: {url}"))
    }
}

fn resolve_target(path: &str) -> Result<DbTarget> {
    let path = normalize_path(path)?;
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err(anyhow!("db 路径需要形如 local/db/table"));
    }

    if let Some((url, max_connections)) = connection_config(&path) {
        return Ok(DbTarget { pool_path: path, url, table: None, max_connections });
    }

    for split in (1..parts.len()).rev() {
        let pool_path = parts[..split].join("/");
        let table = parts[split..].join("/");
        if let Some((url, max_connections)) = connection_config(&pool_path) {
            return Ok(DbTarget { pool_path, url, table: Some(table), max_connections });
        }
    }

    Err(anyhow!("未找到 db 连接 URL: {}", path))
}

fn normalize_path(path: &str) -> Result<String> {
    let path = path.trim().trim_matches('/');
    if path.is_empty() || path.split('/').any(str::is_empty) {
        return Err(anyhow!("非法 db 路径: {path:?}"));
    }
    Ok(path.to_string())
}

fn connection_config(path: &str) -> Option<(String, u32)> {
    let value = root::get(path).ok()?;
    if value.is_str() {
        return Some((value.as_str().to_string(), 5));
    }

    if value.is_map() {
        let url = value.get_dynamic("url").or_else(|| value.get_dynamic("database_url")).or_else(|| value.get_dynamic("连接")).filter(Dynamic::is_str).map(|url| url.as_str().to_string())?;
        let max_connections = value.get_dynamic("max_connections").and_then(|v| v.as_int()).unwrap_or(5).clamp(1, 1024) as u32;
        return Some((url, max_connections));
    }

    None
}

fn parse_columns(fields: &Dynamic) -> Result<Vec<ColumnDef>> {
    if fields.is_map() {
        let mut columns = Vec::new();
        for key in fields.keys() {
            if is_index_key(key.as_str()) {
                continue;
            }
            let value = fields.get_dynamic(key.as_str()).unwrap_or(Dynamic::Null);
            columns.push(parse_map_column(key.as_str(), &value)?);
        }
        return Ok(columns);
    }

    if fields.is_list() {
        let mut columns = Vec::new();
        for idx in 0..fields.len() {
            if let Some(item) = fields.get_idx(idx) {
                columns.push(parse_list_column(&item)?);
            }
        }
        return Ok(columns);
    }

    Err(anyhow!("db::create 字段需要 map 或 list"))
}

fn parse_indexes(fields: &Dynamic) -> Result<Vec<IndexDef>> {
    let mut indexes = Vec::new();
    if fields.is_map() {
        for key in ["@index", "@indexes", "index", "indexes", "索引"] {
            if let Some(value) = fields.get_dynamic(key) {
                indexes.extend(parse_index_defs(&value)?);
            }
        }
    }
    Ok(indexes)
}

fn parse_index_defs(value: &Dynamic) -> Result<Vec<IndexDef>> {
    if value.is_null() {
        return Ok(Vec::new());
    }

    if value.is_str() || value.is_map() {
        return Ok(vec![parse_index_def(value)?]);
    }

    if value.is_list() {
        if looks_like_column_list(value) {
            return Ok(vec![parse_index_def(value)?]);
        }

        let mut indexes = Vec::new();
        for idx in 0..value.len() {
            if let Some(item) = value.get_idx(idx) {
                indexes.push(parse_index_def(&item)?);
            }
        }
        return Ok(indexes);
    }

    Err(anyhow!("索引定义需要字符串、list 或 map"))
}

fn parse_index_def(value: &Dynamic) -> Result<IndexDef> {
    if value.is_str() {
        return Ok(IndexDef { name: None, columns: vec![value.as_str().to_string()], unique: false });
    }

    if value.is_list() {
        return Ok(IndexDef { name: None, columns: parse_column_names(value)?, unique: false });
    }

    if value.is_map() {
        let name = dynamic_string(value, &["name", "index", "索引名"]);
        let unique = value.get_dynamic("unique").or_else(|| value.get_dynamic("唯一")).is_some_and(|value| value.is_true());
        let columns = value.get_dynamic("columns").or_else(|| value.get_dynamic("fields")).or_else(|| value.get_dynamic("cols")).or_else(|| value.get_dynamic("字段")).ok_or_else(|| anyhow!("索引定义缺少 columns"))?;
        return Ok(IndexDef { name, columns: parse_column_names(&columns)?, unique });
    }

    Err(anyhow!("非法索引定义: {value:?}"))
}

fn parse_column_names(value: &Dynamic) -> Result<Vec<String>> {
    if value.is_str() {
        return Ok(value.as_str().split(',').map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect());
    }

    if value.is_list() {
        let mut columns = Vec::new();
        for idx in 0..value.len() {
            let Some(item) = value.get_idx(idx) else {
                continue;
            };
            if !item.is_str() {
                return Err(anyhow!("索引字段名需要字符串"));
            }
            columns.push(item.as_str().to_string());
        }
        return Ok(columns);
    }

    Err(anyhow!("索引字段需要字符串或 list"))
}

fn looks_like_column_list(value: &Dynamic) -> bool {
    value.len() > 0 && (0..value.len()).all(|idx| value.get_idx(idx).is_some_and(|item| item.is_str()))
}

fn is_index_key(key: &str) -> bool {
    matches!(key, "@index" | "@indexes" | "index" | "indexes" | "索引")
}

fn parse_map_column(default_name: &str, value: &Dynamic) -> Result<ColumnDef> {
    if value.is_str() {
        return Ok(ColumnDef { name: default_name.to_string(), ty: value.as_str().to_string() });
    }

    if value.is_map() {
        let name = dynamic_string(value, &["name", "field", "字段", "字段名"]).unwrap_or_else(|| default_name.to_string());
        let ty = dynamic_string(value, &["type", "ty", "类型"]).ok_or_else(|| anyhow!("字段 {name} 缺少 type"))?;
        return Ok(ColumnDef { name, ty });
    }

    Err(anyhow!("字段 {default_name} 类型需要字符串或 map"))
}

fn parse_list_column(value: &Dynamic) -> Result<ColumnDef> {
    if value.is_map() {
        let name = dynamic_string(value, &["name", "field", "字段", "字段名"]).ok_or_else(|| anyhow!("字段缺少 name"))?;
        let ty = dynamic_string(value, &["type", "ty", "类型"]).ok_or_else(|| anyhow!("字段 {name} 缺少 type"))?;
        return Ok(ColumnDef { name, ty });
    }

    if value.is_list() && value.len() >= 2 {
        let name = value.get_idx(0).filter(Dynamic::is_str).map(|v| v.as_str().to_string()).ok_or_else(|| anyhow!("字段名需要字符串"))?;
        let ty = value.get_idx(1).filter(Dynamic::is_str).map(|v| v.as_str().to_string()).ok_or_else(|| anyhow!("字段类型需要字符串"))?;
        return Ok(ColumnDef { name, ty });
    }

    if value.is_str() {
        let Some((name, ty)) = value.as_str().split_once(char::is_whitespace) else {
            return Err(anyhow!("字符串字段需要形如 \"name TYPE\""));
        };
        return Ok(ColumnDef { name: name.trim().to_string(), ty: ty.trim().to_string() });
    }

    Err(anyhow!("list 字段需要 map、二元 list 或 \"name TYPE\" 字符串"))
}

fn build_create_sql(table: &str, columns: &[ColumnDef], kind: DbKind) -> Result<String> {
    let table = quote_ident(table, kind)?;
    let columns = columns.iter().map(|column| Ok(format!("{} {}", quote_ident(&column.name, kind)?, checked_type(&column.ty)?))).collect::<Result<Vec<_>>>()?.join(", ");
    Ok(format!("CREATE TABLE IF NOT EXISTS {table} ({columns})"))
}

fn build_create_index_sql(table: &str, index: &IndexDef, kind: DbKind) -> Result<String> {
    if index.columns.is_empty() {
        return Err(anyhow!("索引至少需要一个字段"));
    }
    let index_name = match &index.name {
        Some(name) => name.clone(),
        None => format!("idx_{}_{}", sanitize_index_part(table), index.columns.iter().map(|column| sanitize_index_part(column)).collect::<Vec<_>>().join("_")),
    };
    let unique = if index.unique { "UNIQUE " } else { "" };
    let table = quote_ident(table, kind)?;
    let index_name = quote_ident(&index_name, kind)?;
    let columns = index.columns.iter().map(|column| quote_ident(column, kind)).collect::<Result<Vec<_>>>()?.join(", ");
    Ok(format!("CREATE {unique}INDEX IF NOT EXISTS {index_name} ON {table} ({columns})"))
}

fn sanitize_index_part(value: &str) -> String {
    let mut out = value.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' { ch } else { '_' }).collect::<String>();
    if out.is_empty() {
        out.push_str("x");
    }
    out
}

fn parse_transaction_steps(steps: &Dynamic) -> Result<Vec<(String, Dynamic)>> {
    if !steps.is_list() {
        return Err(anyhow!("db::transaction 需要 [[sql, data], ...]"));
    }

    let mut out = Vec::new();
    for idx in 0..steps.len() {
        let Some(step) = steps.get_idx(idx) else {
            continue;
        };
        out.push(parse_transaction_step(&step)?);
    }
    Ok(out)
}

fn parse_transaction_step(step: &Dynamic) -> Result<(String, Dynamic)> {
    if step.is_list() {
        let sql = step.get_idx(0).filter(Dynamic::is_str).map(|value| value.as_str().to_string()).ok_or_else(|| anyhow!("事务步骤缺少 SQL 字符串"))?;
        let data = step.get_idx(1).unwrap_or(Dynamic::Null);
        return Ok((sql, data));
    }

    if step.is_map() {
        let sql = dynamic_string(step, &["sql", "SQL"]).ok_or_else(|| anyhow!("事务步骤缺少 sql"))?;
        let data = step.get_dynamic("data").or_else(|| step.get_dynamic("value")).or_else(|| step.get_dynamic("Value")).unwrap_or(Dynamic::Null);
        return Ok((sql, data));
    }

    Err(anyhow!("事务步骤需要 [sql, data] 或 {{sql, data}}"))
}

fn prepare_sql(sql: &str, data: Dynamic, kind: DbKind) -> Result<(String, Vec<Dynamic>)> {
    if data.is_null() {
        return Ok((rewrite_ordered_placeholders(sql, kind), Vec::new()));
    }

    if data.is_map() {
        let (sql, names) = rewrite_named_placeholders(sql, kind)?;
        let mut values = Vec::with_capacity(names.len());
        for name in names {
            let value = data.get_dynamic(&name).ok_or_else(|| anyhow!("SQL 参数缺少字段: {name}"))?;
            values.push(value);
        }
        return Ok((sql, values));
    }

    if data.is_list() {
        let values = (0..data.len()).filter_map(|idx| data.get_idx(idx)).collect();
        return Ok((rewrite_ordered_placeholders(sql, kind), values));
    }

    Ok((rewrite_ordered_placeholders(sql, kind), vec![data]))
}

fn bind_values<'q>(mut query: Query<'q, Any, AnyArguments<'q>>, values: Vec<Dynamic>) -> Result<Query<'q, Any, AnyArguments<'q>>> {
    for value in values {
        query = bind_value(query, value)?;
    }
    Ok(query)
}

fn bind_value<'q>(query: Query<'q, Any, AnyArguments<'q>>, value: Dynamic) -> Result<Query<'q, Any, AnyArguments<'q>>> {
    match value {
        Dynamic::Null => Ok(query.bind(Option::<i32>::None)),
        Dynamic::Bool(value) => Ok(query.bind(value)),
        Dynamic::U8(value) => Ok(query.bind(value as i16)),
        Dynamic::I8(value) => Ok(query.bind(value as i16)),
        Dynamic::U16(value) => Ok(query.bind(value as i32)),
        Dynamic::I16(value) => Ok(query.bind(value)),
        Dynamic::U32(value) => Ok(query.bind(value as i64)),
        Dynamic::I32(value) => Ok(query.bind(value)),
        Dynamic::U64(value) => {
            let value = i64::try_from(value).map_err(|_| anyhow!("u64 参数超过 SQL i64 范围"))?;
            Ok(query.bind(value))
        }
        Dynamic::I64(value) => Ok(query.bind(value)),
        Dynamic::F32(value) => Ok(query.bind(value)),
        Dynamic::F64(value) => Ok(query.bind(value)),
        Dynamic::String(value) => Ok(query.bind(value.to_string())),
        Dynamic::StringBuf(value) => Ok(query.bind(value)),
        Dynamic::Bytes(value) => Ok(query.bind(value)),
        value => Err(anyhow!("不支持的 SQL 绑定值: {value:?}")),
    }
}

fn row_to_dynamic(row: AnyRow) -> Dynamic {
    let mut out = BTreeMap::<SmolStr, Dynamic>::new();
    for (idx, column) in row.columns().iter().enumerate() {
        let value = row_value_to_dynamic(&row, idx, column.type_info().kind()).unwrap_or(Dynamic::Null);
        out.insert(column.name().into(), value);
    }
    Dynamic::map(out)
}

fn row_value_to_dynamic(row: &AnyRow, idx: usize, kind: AnyTypeInfoKind) -> Option<Dynamic> {
    if row.try_get_raw(idx).ok().is_some_and(|value| value.is_null()) {
        return Some(Dynamic::Null);
    }

    match kind {
        AnyTypeInfoKind::Null => Some(Dynamic::Null),
        AnyTypeInfoKind::Bool => row.try_get::<bool, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::SmallInt => row.try_get::<i16, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::Integer => row.try_get::<i32, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::BigInt => row.try_get::<i64, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::Real => row.try_get::<f32, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::Double => row.try_get::<f64, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::Text => row.try_get::<String, _>(idx).ok().map(Into::into),
        AnyTypeInfoKind::Blob => row.try_get::<Vec<u8>, _>(idx).ok().map(Into::into),
    }
}

fn rewrite_named_placeholders(sql: &str, kind: DbKind) -> Result<(String, Vec<String>)> {
    let mut out = String::with_capacity(sql.len());
    let mut names = Vec::new();
    let mut scanner = SqlScanner::new(sql);

    while let Some(token) = scanner.next_token() {
        match token {
            SqlToken::Text(text) => out.push_str(text),
            SqlToken::OrderedPlaceholder => out.push_str(&placeholder(kind, names.len() + 1)),
            SqlToken::NamedPlaceholder(name) => {
                names.push(name.to_string());
                out.push_str(&placeholder(kind, names.len()));
            }
        }
    }

    Ok((out, names))
}

fn rewrite_ordered_placeholders(sql: &str, kind: DbKind) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut index = 0usize;
    let mut scanner = SqlScanner::new(sql);

    while let Some(token) = scanner.next_token() {
        match token {
            SqlToken::Text(text) => out.push_str(text),
            SqlToken::OrderedPlaceholder => {
                index += 1;
                out.push_str(&placeholder(kind, index));
            }
            SqlToken::NamedPlaceholder(name) => {
                out.push(':');
                out.push_str(name);
            }
        }
    }

    out
}

fn placeholder(kind: DbKind, index: usize) -> String {
    match kind {
        DbKind::Postgres => format!("${index}"),
        DbKind::MySql => "?".to_string(),
    }
}

enum SqlToken<'a> {
    Text(&'a str),
    OrderedPlaceholder,
    NamedPlaceholder(&'a str),
}

struct SqlScanner<'a> {
    sql: &'a str,
    pos: usize,
}

impl<'a> SqlScanner<'a> {
    fn new(sql: &'a str) -> Self {
        Self { sql, pos: 0 }
    }

    fn next_token(&mut self) -> Option<SqlToken<'a>> {
        if self.pos >= self.sql.len() {
            return None;
        }

        let start = self.pos;
        while self.pos < self.sql.len() {
            let rest = &self.sql[self.pos..];
            if rest.starts_with('\'') {
                self.skip_quoted('\'');
            } else if rest.starts_with('"') {
                self.skip_quoted('"');
            } else if rest.starts_with('`') {
                self.skip_quoted('`');
            } else if rest.starts_with("--") {
                self.skip_line_comment();
            } else if rest.starts_with("/*") {
                self.skip_block_comment();
            } else if rest.starts_with('?') {
                if start < self.pos {
                    return Some(SqlToken::Text(&self.sql[start..self.pos]));
                }
                self.pos += 1;
                return Some(SqlToken::OrderedPlaceholder);
            } else if rest.starts_with(':') && !rest.starts_with("::") {
                if let Some(name_len) = named_param_len(&rest[1..]) {
                    if start < self.pos {
                        return Some(SqlToken::Text(&self.sql[start..self.pos]));
                    }
                    let name_start = self.pos + 1;
                    self.pos = name_start + name_len;
                    return Some(SqlToken::NamedPlaceholder(&self.sql[name_start..self.pos]));
                }
                self.pos += 1;
            } else {
                self.pos += rest.chars().next().map(char::len_utf8).unwrap_or(1);
            }
        }

        Some(SqlToken::Text(&self.sql[start..self.pos]))
    }

    fn skip_quoted(&mut self, quote: char) {
        self.pos += quote.len_utf8();
        while self.pos < self.sql.len() {
            let rest = &self.sql[self.pos..];
            if rest.starts_with(quote) {
                self.pos += quote.len_utf8();
                if quote == '\'' && self.sql[self.pos..].starts_with('\'') {
                    self.pos += quote.len_utf8();
                    continue;
                }
                break;
            }
            self.pos += rest.chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }

    fn skip_line_comment(&mut self) {
        self.pos += 2;
        while self.pos < self.sql.len() {
            let rest = &self.sql[self.pos..];
            self.pos += rest.chars().next().map(char::len_utf8).unwrap_or(1);
            if rest.starts_with('\n') {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) {
        self.pos += 2;
        while self.pos < self.sql.len() {
            if self.sql[self.pos..].starts_with("*/") {
                self.pos += 2;
                break;
            }
            let rest = &self.sql[self.pos..];
            self.pos += rest.chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }
}

fn named_param_len(input: &str) -> Option<usize> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return None;
    }

    let mut len = first.len_utf8();
    for (idx, ch) in chars {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            len = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    Some(len)
}

fn dynamic_string(value: &Dynamic, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| value.get_dynamic(key).filter(Dynamic::is_str).map(|value| value.as_str().to_string()))
}

fn quote_ident(name: &str, kind: DbKind) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('\0') {
        return Err(anyhow!("非法 SQL 标识符: {name:?}"));
    }
    // MySQL 默认 sql_mode 不含 ANSI_QUOTES,双引号被当字符串字面量,必须用反引号;
    // PostgreSQL/SQLite 用 ANSI 双引号。转义:MySQL 反引号用 `` 转义,PG 双引号用 "" 转义。
    Ok(match kind {
        DbKind::MySql => format!("`{}`", name.replace('`', "``")),
        DbKind::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
    })
}

fn checked_type(ty: &str) -> Result<&str> {
    let ty = ty.trim();
    if ty.is_empty() || ty.contains('\0') || ty.contains(';') || ty.contains("--") || ty.contains("/*") || ty.contains("*/") {
        return Err(anyhow!("非法 SQL 字段类型: {ty:?}"));
    }
    Ok(ty)
}

pub const DB_NATIVE: [(&str, &[Type], Type, *const u8); 5] = [
    ("create", &[Type::Any, Type::Any], Type::Bool, db_create as *const u8),
    ("drop", &[Type::Any], Type::Bool, db_drop as *const u8),
    ("select", &[Type::Any, Type::Any, Type::Any], Type::Any, db_select as *const u8),
    ("exec", &[Type::Any, Type::Any, Type::Any], Type::I64, db_exec as *const u8),
    ("transaction", &[Type::Any, Type::Any], Type::I64, db_transaction as *const u8),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;
    use dynamic::map;

    #[test]
    fn resolves_parent_connection_and_table_suffix() -> Result<()> {
        root::add_value("local/db_module_resolve", "postgres://user:pass@localhost/zust")?;
        let target = resolve_target("local/db_module_resolve/user")?;

        assert_eq!(target.pool_path, "local/db_module_resolve");
        assert_eq!(target.table.as_deref(), Some("user"));
        assert_eq!(target.url, "postgres://user:pass@localhost/zust");
        Ok(())
    }

    #[test]
    fn exact_connection_path_is_database_not_table() -> Result<()> {
        root::add_value("local/db_module_exact/user", "mysql://user:pass@localhost/zust")?;
        let target = resolve_target("local/db_module_exact/user")?;

        assert_eq!(target.pool_path, "local/db_module_exact/user");
        assert_eq!(target.table, None);
        assert_eq!(target.url, "mysql://user:pass@localhost/zust");
        Ok(())
    }

    #[test]
    fn builds_create_sql_from_dynamic_field_map() -> Result<()> {
        let columns = parse_columns(&map!("id"=> "BIGINT PRIMARY KEY", "name"=> "TEXT"))?;
        let sql = build_create_sql("user", &columns, DbKind::Postgres)?;

        assert_eq!(sql, "CREATE TABLE IF NOT EXISTS \"user\" (\"id\" BIGINT PRIMARY KEY, \"name\" TEXT)");
        Ok(())
    }

    #[test]
    fn builds_create_sql_uses_backtick_for_mysql() -> Result<()> {
        // MySQL 默认 sql_mode 不含 ANSI_QUOTES,必须用反引号引用标识符。
        let columns = parse_columns(&map!("id"=> "BIGINT PRIMARY KEY", "name"=> "TEXT"))?;
        let sql = build_create_sql("user", &columns, DbKind::MySql)?;

        assert_eq!(sql, "CREATE TABLE IF NOT EXISTS `user` (`id` BIGINT PRIMARY KEY, `name` TEXT)");
        // 反引号转义
        assert_eq!(quote_ident("with`tick", DbKind::MySql)?, "`with``tick`");
        Ok(())
    }

    #[test]
    fn parses_indexes_from_create_fields() -> Result<()> {
        let fields = map!(
            "id"=> "BIGINT PRIMARY KEY",
            "name"=> "TEXT",
            "email"=> "TEXT",
            "@indexes"=> Dynamic::list(vec![
                "name".into(),
                Dynamic::list(vec!["name".into(), "email".into()]),
                map!("name"=> "uniq_user_email", "columns"=> Dynamic::list(vec!["email".into()]), "unique"=> true),
            ])
        );
        let columns = parse_columns(&fields)?;
        let indexes = parse_indexes(&fields)?;

        assert_eq!(columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>(), vec!["email", "id", "name"]);
        assert_eq!(indexes.len(), 3);
        assert_eq!(build_create_index_sql("user", &indexes[0], DbKind::Postgres)?, "CREATE INDEX IF NOT EXISTS \"idx_user_name\" ON \"user\" (\"name\")");
        assert_eq!(build_create_index_sql("user", &indexes[1], DbKind::Postgres)?, "CREATE INDEX IF NOT EXISTS \"idx_user_name_email\" ON \"user\" (\"name\", \"email\")");
        assert_eq!(build_create_index_sql("user", &indexes[2], DbKind::Postgres)?, "CREATE UNIQUE INDEX IF NOT EXISTS \"uniq_user_email\" ON \"user\" (\"email\")");
        Ok(())
    }

    #[test]
    fn rewrites_named_params_and_collects_map_values() -> Result<()> {
        let data = map!("id"=> 7, "name"=> "zhu");
        let (sql, values) = prepare_sql("select * from user where id = :id and name = :name", data, DbKind::Postgres)?;

        assert_eq!(sql, "select * from user where id = $1 and name = $2");
        assert_eq!(values, vec![Dynamic::from(7), Dynamic::from("zhu")]);
        Ok(())
    }

    #[test]
    fn rewrites_ordered_params_for_postgres_lists() -> Result<()> {
        let data = Dynamic::list(vec![1.into(), "zhu".into()]);
        let (sql, values) = prepare_sql("select * from user where id = ? and name = ?", data, DbKind::Postgres)?;

        assert_eq!(sql, "select * from user where id = $1 and name = $2");
        assert_eq!(values, vec![Dynamic::from(1), Dynamic::from("zhu")]);
        Ok(())
    }

    #[test]
    fn does_not_rewrite_params_inside_literals_or_comments() -> Result<()> {
        let data = map!("id"=> 1);
        let (sql, values) = prepare_sql("select ':id', col from t -- :skip\nwhere id = :id and note = '?'", data, DbKind::MySql)?;

        assert_eq!(sql, "select ':id', col from t -- :skip\nwhere id = ? and note = '?'");
        assert_eq!(values, vec![Dynamic::from(1)]);
        Ok(())
    }

    #[test]
    fn parses_transaction_steps() -> Result<()> {
        let steps =
            Dynamic::list(vec![Dynamic::list(vec!["insert into t values (:id)".into(), map!("id"=> 1)]), map!("sql"=> "update t set name = ? where id = ?", "data"=> Dynamic::list(vec!["zust".into(), 1.into()]))]);
        let parsed = parse_transaction_steps(&steps)?;

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "insert into t values (:id)");
        assert_eq!(parsed[0].1.get_dynamic("id").and_then(|value| value.as_int()), Some(1));
        assert_eq!(parsed[1].0, "update t set name = ? where id = ?");
        assert!(parsed[1].1.is_list());
        Ok(())
    }

    #[test]
    fn postgres_select_and_exec_when_url_is_set() -> Result<()> {
        let Ok(url) = std::env::var("ZUST_TEST_POSTGRES_URL") else {
            eprintln!("skip postgres integration test: set ZUST_TEST_POSTGRES_URL");
            return Ok(());
        };
        root::add_value("local/db_module_postgres_live", url)?;

        root::block_on_async(|| {
            Box::pin(async {
                let table_path = "local/db_module_postgres_live/zust_db_module_live";
                let _ = drop_table(table_path.into()).await;
                assert!(
                    create_table(
                        table_path.into(),
                        map!(
                            "id"=> "BIGINT PRIMARY KEY",
                            "name"=> "TEXT",
                            "@index"=> Dynamic::list(vec!["name".into()])
                        ),
                    )
                    .await?
                );

                let inserted = exec_sql("local/db_module_postgres_live".into(), "insert into zust_db_module_live (id, name) values (:id, :name)".into(), map!("id"=> 1, "name"=> "zhu")).await?;
                assert_eq!(inserted, 1);

                let updated = exec_sql("local/db_module_postgres_live".into(), "update zust_db_module_live set name = ? where id = ?".into(), Dynamic::list(vec!["zust".into(), 1.into()])).await?;
                assert_eq!(updated, 1);

                let rows = select_rows("local/db_module_postgres_live".into(), "select id, name from zust_db_module_live where id = :id".into(), map!("id"=> 1)).await?;

                assert_eq!(rows.len(), 1);
                let row = rows.get_idx(0).expect("postgres row");
                assert_eq!(row.get_dynamic("id").and_then(|value| value.as_int()), Some(1));
                assert_eq!(row.get_dynamic("name").map(|value| value.as_str().to_string()), Some("zust".to_string()));

                assert!(drop_table(table_path.into()).await?);
                Ok(())
            })
        })
    }

    #[test]
    fn postgres_transaction_when_url_is_set() -> Result<()> {
        let Ok(url) = std::env::var("ZUST_TEST_POSTGRES_URL") else {
            eprintln!("skip postgres transaction integration test: set ZUST_TEST_POSTGRES_URL");
            return Ok(());
        };
        root::add_value("local/db_module_postgres_tx", url)?;

        root::block_on_async(|| {
            Box::pin(async {
                let table_path = "local/db_module_postgres_tx/zust_db_module_tx";
                let _ = drop_table(table_path.into()).await;
                assert!(create_table(table_path.into(), map!("id"=> "BIGINT PRIMARY KEY", "name"=> "TEXT")).await?);

                let steps = Dynamic::list(vec![
                    Dynamic::list(vec!["insert into zust_db_module_tx (id, name) values (:id, :name)".into(), map!("id"=> 1, "name"=> "first")]),
                    Dynamic::list(vec!["update zust_db_module_tx set name = ? where id = ?".into(), Dynamic::list(vec!["second".into(), 1.into()])]),
                ]);
                assert_eq!(transaction_sql("local/db_module_postgres_tx".into(), steps).await?, 2);

                let rollback_steps = Dynamic::list(vec![
                    Dynamic::list(vec!["insert into zust_db_module_tx (id, name) values (:id, :name)".into(), map!("id"=> 2, "name"=> "rollback")]),
                    Dynamic::list(vec!["insert into zust_db_module_tx (id, name) values (:id, :name)".into(), map!("id"=> 1, "name"=> "duplicate")]),
                ]);
                assert!(transaction_sql("local/db_module_postgres_tx".into(), rollback_steps).await.is_err());

                let rows = select_rows("local/db_module_postgres_tx".into(), "select id, name from zust_db_module_tx order by id".into(), Dynamic::Null).await?;

                assert_eq!(rows.len(), 1);
                let row = rows.get_idx(0).expect("postgres tx row");
                assert_eq!(row.get_dynamic("id").and_then(|value| value.as_int()), Some(1));
                assert_eq!(row.get_dynamic("name").map(|value| value.as_str().to_string()), Some("second".to_string()));

                assert!(drop_table(table_path.into()).await?);
                Ok(())
            })
        })
    }

    #[test]
    fn postgres_transaction_from_zust_script_when_url_is_set() -> Result<()> {
        let Ok(url) = std::env::var("ZUST_TEST_POSTGRES_URL") else {
            eprintln!("skip postgres transaction VM integration test: set ZUST_TEST_POSTGRES_URL");
            return Ok(());
        };
        root::add_value("local/db_module_postgres_vm_tx", url)?;

        let vm = Vm::with_all()?;
        vm.jit.write().import_code(
            "db_transaction_vm",
            br#"
            pub fn run() {
                db::transaction("local/db_module_postgres_vm_tx", [
                    ["drop table if exists zust_db_module_vm_tx", null],
                    ["create table zust_db_module_vm_tx (id BIGINT PRIMARY KEY, name TEXT)", null],
                    ["insert into zust_db_module_vm_tx (id, name) values (:id, :name)", {id: 1, name: "script"}],
                    ["update zust_db_module_vm_tx set name = ? where id = ?", ["script-updated", 1]]
                ])
            }
            "#
            .to_vec(),
        )?;

        let (ptr, ret) = vm.jit.write().get_fn_ptr("db_transaction_vm::run", &[])?;
        assert_eq!(ret, Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
        assert_eq!(run(), 2);

        root::block_on_async(|| {
            Box::pin(async {
                let rows = select_rows("local/db_module_postgres_vm_tx".into(), "select id, name from zust_db_module_vm_tx where id = :id".into(), map!("id"=> 1)).await?;

                assert_eq!(rows.len(), 1);
                let row = rows.get_idx(0).expect("postgres VM tx row");
                assert_eq!(row.get_dynamic("id").and_then(|value| value.as_int()), Some(1));
                assert_eq!(row.get_dynamic("name").map(|value| value.as_str().to_string()), Some("script-updated".to_string()));

                let _ = exec_sql("local/db_module_postgres_vm_tx".into(), "drop table if exists zust_db_module_vm_tx".into(), Dynamic::Null).await?;
                Ok(())
            })
        })
    }
}
