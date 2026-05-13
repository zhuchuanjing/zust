use anyhow::Result;
use redis::Commands;
use smol_str::SmolStr;

const DIR_KEY_PREFIX: &str = "__zust_root_dir__:";
const DIR_REBUILT_KEY: &str = "__zust_root_dir_rebuilt__";

pub fn is_internal_key(key: &str) -> bool {
    key.starts_with(DIR_KEY_PREFIX) || key == DIR_REBUILT_KEY
}

fn normalize_path(path: &str) -> &str {
    path.trim_matches('/')
}

fn dir_key(path: &str) -> String {
    format!("{DIR_KEY_PREFIX}{}", normalize_path(path))
}

fn parent_child(path: &str) -> Option<(&str, &str)> {
    let path = normalize_path(path);
    if path.is_empty() {
        return None;
    }
    path.rsplit_once('/').map_or(Some(("", path)), |(parent, child)| Some((parent, child)))
}

pub fn add_path(conn: &mut redis::Connection, path: &str) -> Result<()> {
    let path = normalize_path(path);
    if path.is_empty() || is_internal_key(path) {
        return Ok(());
    }

    let mut parent = String::new();
    for child in path.split('/').filter(|part| !part.is_empty()) {
        let _: usize = conn.sadd(dir_key(&parent), child)?;
        if parent.is_empty() {
            parent.push_str(child);
        } else {
            parent.push('/');
            parent.push_str(child);
        }
    }
    Ok(())
}

pub fn remove_path(conn: &mut redis::Connection, path: &str) -> Result<()> {
    let path = normalize_path(path);
    if path.is_empty() || is_internal_key(path) || has_index_children(conn, path)? {
        return Ok(());
    }
    remove_empty_entry(conn, path)
}

fn remove_empty_entry(conn: &mut redis::Connection, path: &str) -> Result<()> {
    let Some((parent, child)) = parent_child(path) else {
        return Ok(());
    };

    let parent_key = dir_key(parent);
    let _: usize = conn.srem(&parent_key, child)?;
    if conn.scard::<_, usize>(&parent_key)? != 0 {
        return Ok(());
    }

    let _: usize = conn.del(&parent_key)?;
    if !parent.is_empty() && !conn.exists::<_, bool>(parent)? {
        remove_empty_entry(conn, parent)?;
    }
    Ok(())
}

fn has_index_children(conn: &mut redis::Connection, path: &str) -> Result<bool> {
    Ok(conn.scard::<_, usize>(dir_key(path))? != 0)
}

pub fn rebuild(conn: &mut redis::Connection) -> Result<()> {
    let options = redis::ScanOptions::default().with_count(1000);
    let mut paths = Vec::new();
    let mut index_keys = Vec::new();
    for key in conn.scan_options::<String>(options)? {
        let key = key?;
        if is_internal_key(&key) {
            index_keys.push(key);
        } else {
            paths.push(key);
        }
    }
    if !index_keys.is_empty() {
        let _: usize = conn.del(index_keys)?;
    }
    for path in paths {
        add_path(conn, &path)?;
    }
    Ok(())
}

pub fn rebuild_once(conn: &mut redis::Connection) -> Result<()> {
    if conn.exists::<_, bool>(DIR_REBUILT_KEY)? {
        return Ok(());
    }
    rebuild(conn)?;
    Ok(conn.set::<_, _, ()>(DIR_REBUILT_KEY, b"1")?)
}

pub fn children(conn: &mut redis::Connection, path: &str) -> Result<Vec<SmolStr>> {
    let children: Vec<String> = conn.smembers(dir_key(path))?;
    Ok(children.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_child_handles_nested_paths() {
        assert_eq!(parent_child("a/b/c"), Some(("a/b", "c")));
        assert_eq!(parent_child("a"), Some(("", "a")));
        assert_eq!(parent_child("a/"), Some(("", "a")));
        assert_eq!(parent_child(""), None);
    }
}
