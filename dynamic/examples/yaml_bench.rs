use std::collections::BTreeMap;
use std::time::Instant;

use dynamic::{Dynamic, ToYaml};

fn gen_node(depth: i64, idx: i64) -> Dynamic {
    let mut m: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    m.insert("id".into(), Dynamic::I64(idx));
    m.insert("name".into(), Dynamic::String(format!("user_{idx}").into()));
    m.insert(
        "email".into(),
        Dynamic::String(format!("user{idx}@example.com").into()),
    );
    m.insert("active".into(), Dynamic::Bool(idx % 3 == 0));
    m.insert("score".into(), Dynamic::F64((idx as f64) * 7.0 / 13.0));
    let tags = vec![
        Dynamic::String(format!("tag_{}", idx % 5).into()),
        Dynamic::String(format!("tag_{}", idx % 7).into()),
    ];
    m.insert("tags".into(), Dynamic::list(tags));
    if depth > 0 && idx % 10 == 0 {
        let mut children = Vec::new();
        for i in 0..3 {
            children.push(gen_node(depth - 1, idx * 1000 + i));
        }
        m.insert("children".into(), Dynamic::list(children));
    } else {
        m.insert("note".into(), Dynamic::String("leaf node".into()));
    }
    Dynamic::map(m)
}

fn gen_dataset(n: i64) -> Dynamic {
    let mut root: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    root.insert("version".into(), Dynamic::String("1.0".into()));
    root.insert(
        "generated_at".into(),
        Dynamic::String("2026-07-08T19:51:00Z".into()),
    );
    root.insert("count".into(), Dynamic::I64(n));
    let mut users = Vec::new();
    for i in 0..n {
        users.push(gen_node(0, i));
    }
    root.insert("users".into(), Dynamic::list(users));
    Dynamic::map(root)
}

fn main() {
    for &size in &[1_000_i64, 10_000, 100_000] {
        let t0 = Instant::now();
        let data = gen_dataset(size);
        let build_ms = t0.elapsed().as_millis();

        let t1 = Instant::now();
        let yaml = data.to_yaml_string();
        let emit_ms = t1.elapsed().as_millis();
        let yaml_len = yaml.len();

        let t2 = Instant::now();
        let back = match Dynamic::from_yaml_buf(yaml.as_bytes()) {
            Ok(v) => v,
            Err(e) => {
                println!("parse error: {e}");
                return;
            }
        };
        let parse_ms = t2.elapsed().as_millis();

        let back_count = back.get_dynamic("count").and_then(|v| v.as_int());
        let back_users = back.get_dynamic("users");
        let back_users_len = back_users.as_ref().map(|v| v.len()).unwrap_or(0);
        let ok = back_count == Some(size) && back_users_len as i64 == size;

        println!("---");
        println!("size:        {}", size);
        println!("build:       {} ms", build_ms);
        println!(
            "to_yaml:     {} ms ({} bytes, {:.1} bytes/node)",
            emit_ms,
            yaml_len,
            yaml_len as f64 / size as f64
        );
        println!(
            "from_yaml:   {} ms ({:.0} bytes/ms)",
            parse_ms,
            yaml_len as f64 / parse_ms.max(1) as f64
        );
        println!(
            "round-trip:  {}",
            if ok { "ok" } else { "FAIL" }
        );
    }
}
