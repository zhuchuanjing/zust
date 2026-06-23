use anyhow::{Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::{
    bert::{BertModel, Config as BertConfig},
    qwen2::{Config as Qwen2Config, Model as Qwen2Model},
};
use dynamic::{Dynamic, map};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

pub fn embed(options: Dynamic, input: Dynamic) -> Result<Dynamic> {
    if let Some(embedder) = options.as_custom::<Embedder>() {
        return embedder.embed(input);
    }

    let request = EmbedRequest::from_dynamic(options, input)?;
    let embedder = Embedder::load(request.options)?;
    embedder.embed_texts(request.texts)
}

pub fn load_embedder(options: Dynamic) -> Result<Dynamic> {
    Ok(Dynamic::custom(Embedder::load(EmbedderOptions::from_dynamic(options)?)?))
}

pub struct Embedder {
    tokenizer: Tokenizer,
    model: EmbedderModel,
    device: Device,
    model_path: PathBuf,
    normalize: bool,
    output_dim: Option<usize>,
}

impl Embedder {
    fn load(options: EmbedderOptions) -> Result<Self> {
        let tokenizer = tokenizer(&options.tokenizer_path, options.max_len)?;
        let config = config(&options.config_path)?;
        let device = device(&options.device)?;
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[options.model_path.as_path()], DType::F32, &device)? };
        let model = match config {
            EmbedderConfig::Bert(config) => EmbedderModel::Bert(BertModel::load(vb, &config)?),
            EmbedderConfig::Qwen2(config) => {
                let vb = vb.rename_f(|name| name.strip_prefix("model.").unwrap_or(name).to_string());
                EmbedderModel::Qwen2(Mutex::new(Qwen2Model::new(&config, vb)?))
            }
        };
        Ok(Self { tokenizer, model, device, model_path: options.model_path, normalize: options.normalize, output_dim: options.output_dim })
    }

    fn embed(&self, input: Dynamic) -> Result<Dynamic> {
        self.embed_texts(texts(input)?)
    }

    fn embed_texts(&self, texts: Vec<String>) -> Result<Dynamic> {
        let encodings = self.tokenizer.encode_batch(texts, true).map_err(|err| anyhow!("tokenize failed: {err}"))?;
        let mut input_ids = Vec::with_capacity(encodings.len());
        let mut token_type_ids = Vec::with_capacity(encodings.len());
        let mut masks = Vec::with_capacity(encodings.len());
        for encoding in encodings {
            input_ids.push(encoding.get_ids().to_vec());
            token_type_ids.push(encoding.get_type_ids().to_vec());
            masks.push(encoding.get_attention_mask().to_vec());
        }

        let input_ids = Tensor::new(input_ids, &self.device)?;
        let attention_mask = Tensor::new(masks.clone(), &self.device)?;
        let hidden = match &self.model {
            EmbedderModel::Bert(model) => {
                let token_type_ids = Tensor::new(token_type_ids, &self.device)?;
                model.forward(&input_ids, &token_type_ids, Some(&attention_mask))?
            }
            EmbedderModel::Qwen2(model) => {
                let mut model = model.lock().map_err(|_| anyhow!("qwen2 model lock poisoned"))?;
                model.clear_kv_cache();
                model.forward(&input_ids, 0, Some(&attention_mask))?
            }
        };
        let embeddings = mean_pool(hidden.to_vec3::<f32>()?, &masks, self.normalize, self.output_dim);
        let dim = embeddings.first().map(|row| row.len()).unwrap_or(0);
        let rows: Vec<Dynamic> = embeddings.into_iter().map(|row| Dynamic::from(row.as_slice())).collect();

        Ok(map!(
            "ok"=> true,
            "model"=> self.model_path.to_string_lossy().to_string(),
            "count"=> rows.len() as i64,
            "dim"=> dim as i64,
            "embeddings"=> Dynamic::list(rows)
        ))
    }
}

enum EmbedderModel {
    Bert(BertModel),
    Qwen2(Mutex<Qwen2Model>),
}

struct EmbedRequest {
    options: EmbedderOptions,
    texts: Vec<String>,
}

impl EmbedRequest {
    fn from_dynamic(options: Dynamic, input: Dynamic) -> Result<Self> {
        Ok(Self { options: EmbedderOptions::from_dynamic(options)?, texts: texts(input)? })
    }
}

struct EmbedderOptions {
    model_path: PathBuf,
    tokenizer_path: PathBuf,
    config_path: PathBuf,
    device: String,
    max_len: usize,
    normalize: bool,
    output_dim: Option<usize>,
}

impl EmbedderOptions {
    fn from_dynamic(options: Dynamic) -> Result<Self> {
        if !options.is_map() {
            return Err(anyhow!("candle embed options must be map"));
        }
        let model_path = path_field(&options, &["model", "model_path"])?;
        let tokenizer_path = path_field(&options, &["tokenizer", "tokenizer_path"])?;
        let config_path = path_field(&options, &["config", "config_path"])?;
        let device = options.get_dynamic("device").map(|value| value.as_str().to_string()).unwrap_or_else(|| "cpu".to_string());
        let max_len = options.get_dynamic("max_len").or_else(|| options.get_dynamic("max_length")).and_then(|value| value.as_int()).unwrap_or(512).clamp(1, 8192) as usize;
        let normalize = options.get_dynamic("normalize").and_then(|value| value.as_bool()).unwrap_or(true);
        let output_dim = options.get_dynamic("output_dim").or_else(|| options.get_dynamic("dim")).and_then(|value| value.as_int()).and_then(|value| usize::try_from(value).ok()).filter(|value| *value > 0);
        Ok(Self { model_path, tokenizer_path, config_path, device, max_len, normalize, output_dim })
    }
}

fn path_field(options: &Dynamic, keys: &[&str]) -> Result<PathBuf> {
    for key in keys {
        if let Some(value) = options.get_dynamic(key) {
            let path = value.as_str();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err(anyhow!("missing candle embed path field {}", keys[0]))
}

fn texts(input: Dynamic) -> Result<Vec<String>> {
    if input.is_str() {
        return Ok(vec![input.as_str().to_string()]);
    }
    if !input.is_list() {
        return Err(anyhow!("candle embed input must be string or string list"));
    }
    let mut texts = Vec::with_capacity(input.len());
    for idx in 0..input.len() {
        let item = input.get_idx(idx).ok_or_else(|| anyhow!("missing input item {idx}"))?;
        if !item.is_str() {
            return Err(anyhow!("candle embed input item {idx} must be string"));
        }
        texts.push(item.as_str().to_string());
    }
    Ok(texts)
}

fn tokenizer(path: &Path, max_len: usize) -> Result<Tokenizer> {
    let mut tokenizer = Tokenizer::from_file(path).map_err(|err| anyhow!("load tokenizer {} failed: {err}", path.display()))?;
    tokenizer.with_padding(Some(PaddingParams::default()));
    tokenizer.with_truncation(Some(TruncationParams { max_length: max_len, ..Default::default() })).map_err(|err| anyhow!("set tokenizer truncation failed: {err}"))?;
    Ok(tokenizer)
}

enum EmbedderConfig {
    Bert(BertConfig),
    Qwen2(Qwen2Config),
}

fn config(path: &Path) -> Result<EmbedderConfig> {
    let text = std::fs::read_to_string(path).map_err(|err| anyhow!("read config {} failed: {err}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&text).map_err(|err| anyhow!("parse config {} failed: {err}", path.display()))?;
    let model_type = value.get("model_type").and_then(|value| value.as_str()).unwrap_or("bert");
    match model_type {
        "qwen2" => {
            if value.get("sliding_window").is_none_or(|value| value.is_null()) {
                let max_position_embeddings = value.get("max_position_embeddings").and_then(|value| value.as_u64()).unwrap_or(32768);
                value["sliding_window"] = serde_json::Value::from(max_position_embeddings);
            }
            Ok(EmbedderConfig::Qwen2(serde_json::from_value(value).map_err(|err| anyhow!("parse qwen2 config {} failed: {err}", path.display()))?))
        }
        _ => Ok(EmbedderConfig::Bert(serde_json::from_value(value).map_err(|err| anyhow!("parse bert config {} failed: {err}", path.display()))?)),
    }
}

fn device(name: &str) -> Result<Device> {
    match name {
        "" | "cpu" => Ok(Device::Cpu),
        other => Err(anyhow!("unsupported candle device {other}; this build supports cpu")),
    }
}

fn mean_pool(hidden: Vec<Vec<Vec<f32>>>, masks: &[Vec<u32>], normalize: bool, output_dim: Option<usize>) -> Vec<Vec<f32>> {
    hidden
        .into_iter()
        .zip(masks.iter())
        .map(|(tokens, mask)| {
            let dim = tokens.first().map(|row| row.len()).unwrap_or(0);
            let mut pooled = vec![0f32; dim];
            let mut count = 0f32;
            for (token, keep) in tokens.iter().zip(mask.iter()) {
                if *keep == 0 {
                    continue;
                }
                count += 1.0;
                for (dst, value) in pooled.iter_mut().zip(token.iter()) {
                    *dst += *value;
                }
            }
            if count > 0.0 {
                for value in &mut pooled {
                    *value /= count;
                }
            }
            if let Some(output_dim) = output_dim {
                pooled.truncate(output_dim.min(pooled.len()));
            }
            if normalize {
                normalize_l2(&mut pooled);
            }
            pooled
        })
        .collect()
}

fn normalize_l2(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in values {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_accepts_string_and_string_list() -> Result<()> {
        assert_eq!(texts("hello".into())?, vec!["hello".to_string()]);
        assert_eq!(texts(Dynamic::list(vec!["a".into(), "b".into()]))?, vec!["a".to_string(), "b".to_string()]);
        assert!(texts(Dynamic::list(vec![1i64.into()])).is_err());
        Ok(())
    }

    #[test]
    fn mean_pool_uses_attention_mask() {
        let hidden = vec![vec![vec![1.0, 3.0], vec![3.0, 5.0], vec![99.0, 99.0]]];
        let masks = vec![vec![1, 1, 0]];
        let pooled = mean_pool(hidden, &masks, false, None);
        assert_eq!(pooled, vec![vec![2.0, 4.0]]);
    }

    #[test]
    fn mean_pool_truncates_before_normalize() {
        let hidden = vec![vec![vec![3.0, 4.0, 12.0]]];
        let masks = vec![vec![1]];
        let pooled = mean_pool(hidden, &masks, true, Some(2));
        assert_eq!(pooled, vec![vec![0.6, 0.8]]);
    }
}
