use std::path::PathBuf;

use anyhow::{Context, Result};

const DEFAULT_NODE_ID: &str = "90acbf43a92f21238b811122fba4b6e8e2331f7de0393eb6919331a1df437558";
const DEFAULT_DIR: &str = "dongli-editor/assets";

fn main() -> Result<()> {
    let mut node_id = DEFAULT_NODE_ID.to_string();
    let mut dir = PathBuf::from(DEFAULT_DIR);
    let mut path = String::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--node-id" => {
                if let Some(value) = args.next() {
                    node_id = value;
                }
            }
            "--dir" => {
                if let Some(value) = args.next() {
                    dir = PathBuf::from(value);
                }
            }
            "--path" => {
                if let Some(value) = args.next() {
                    path = value;
                }
            }
            "--help" | "-h" => {
                println!("zust-root-iroh-upload [--node-id NODE_ID] [--dir dongli-editor/assets] [--path PREFIX_OR_FILE]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    let dir = dir.canonicalize().with_context(|| format!("resolve upload dir {}", dir.display()))?;
    let remote = node_id.parse()?;
    let client = root::iroh::IrohClient::with_local_dir(remote, &dir)?;

    println!("iroh upload node: {node_id}");
    println!("iroh upload dir: {}", dir.display());
    println!("iroh upload path: {}", if path.is_empty() { "." } else { path.as_str() });

    let values = client.sync(&path, |progress| {
        println!("[{}/{}] {} {} bytes", progress.current, progress.total, progress.value.id, progress.value.size);
    })?;

    println!("iroh upload done: {} files", values.len());
    Ok(())
}
