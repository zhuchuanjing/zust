use anyhow::{Result, anyhow};
use dynamic::{Dynamic, map};

fn main() -> Result<()> {
    let model_dir = std::env::args().nth(1).unwrap_or_else(|| "models/KaLM-embedding-multilingual-mini-instruct-v2.5".to_string());
    let embedder = llm::candle::load_embedder(map!(
        "model"=> format!("{model_dir}/model.safetensors"),
        "tokenizer"=> format!("{model_dir}/tokenizer.json"),
        "config"=> format!("{model_dir}/config.json"),
        "max_len"=> 128,
        "output_dim"=> 896,
        "normalize"=> true
    ))?;

    let query = "Instruct: Given a query, retrieve documents that answer the query\nQuery: What is Zust?";
    let doc = "Zust is a Rust-like scripting language and runtime written in Rust.";
    let result = llm::candle::embed(embedder, Dynamic::list(vec![query.into(), doc.into()]))?;

    let embeddings = result.get_dynamic("embeddings").ok_or_else(|| anyhow!("missing embeddings"))?;
    let first = embeddings.get_idx(0).ok_or_else(|| anyhow!("missing first embedding"))?;
    let second = embeddings.get_idx(1).ok_or_else(|| anyhow!("missing second embedding"))?;
    let sim = dot(&first, &second)?;

    println!("ok: {}", result.get_dynamic("ok").and_then(|value| value.as_bool()).unwrap_or(false));
    println!("count: {}", result.get_dynamic("count").and_then(|value| value.as_int()).unwrap_or(0));
    println!("dim: {}", result.get_dynamic("dim").and_then(|value| value.as_int()).unwrap_or(0));
    println!("similarity: {:.4}", sim);
    println!("first[0..5]: {:?}", first_values(&first, 5)?);
    Ok(())
}

fn dot(left: &Dynamic, right: &Dynamic) -> Result<f64> {
    let len = left.len().min(right.len());
    let mut total = 0.0;
    for idx in 0..len {
        let left = left.get_idx(idx).and_then(|value| value.as_float()).ok_or_else(|| anyhow!("left embedding item {idx} must be number"))?;
        let right = right.get_idx(idx).and_then(|value| value.as_float()).ok_or_else(|| anyhow!("right embedding item {idx} must be number"))?;
        total += left * right;
    }
    Ok(total)
}

fn first_values(value: &Dynamic, count: usize) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    for idx in 0..count.min(value.len()) {
        values.push(value.get_idx(idx).and_then(|value| value.as_float()).ok_or_else(|| anyhow!("embedding item {idx} must be number"))?);
    }
    Ok(values)
}
