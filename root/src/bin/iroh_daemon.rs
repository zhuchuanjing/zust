use std::path::PathBuf;

use anyhow::Result;
use iroh::SecretKey;

fn main() -> Result<()> {
    let mut root = root::iroh::default_daemon_dir();
    let mut secret_key = None;
    let mut secret_key_file = PathBuf::from(".iroh").join("secret.key");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(value) = args.next() {
                    root = PathBuf::from(value);
                }
            }
            "--secret-key" => {
                if let Some(value) = args.next() {
                    secret_key = Some(root::iroh::parse_secret_key(&value)?);
                }
            }
            "--secret-key-file" => {
                if let Some(value) = args.next() {
                    secret_key_file = PathBuf::from(value);
                }
            }
            "--help" | "-h" => {
                println!("zust-root-iroh-daemon [--root .iroh/daemon] [--secret-key HEX_OR_Z32] [--secret-key-file .iroh/secret.key]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    let secret_key: SecretKey = match secret_key {
        Some(secret_key) => secret_key,
        None => root::iroh::load_or_create_secret_key(secret_key_file)?,
    };

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(root::iroh::run_daemon(root, secret_key))
}
