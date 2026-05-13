use zed_extension_api as zed;

struct ZustExtension;

impl ZustExtension {
    const LANGUAGE_SERVER_ID: &'static str = "zust-lsp";
    const BINARY_NAME: &'static str = "zust-lsp";

    fn language_server_settings(
        &self,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::settings::LspSettings> {
        zed::settings::LspSettings::for_worktree(Self::LANGUAGE_SERVER_ID, worktree)
    }

    fn local_repo_binary(&self, worktree: &zed::Worktree) -> Option<String> {
        worktree
            .read_text_file("zust-lsp/Cargo.toml")
            .ok()
            .map(|_| format!("{}/target/debug/{}", worktree.root_path(), Self::BINARY_NAME))
    }
}

impl zed::Extension for ZustExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        if language_server_id.as_ref() != Self::LANGUAGE_SERVER_ID {
            return Err(format!("unknown language server: {}", language_server_id.as_ref()));
        }

        let settings = self.language_server_settings(worktree)?;
        let binary_settings = settings.binary;

        let command = binary_settings
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .or_else(|| self.local_repo_binary(worktree))
            .or_else(|| worktree.which(Self::BINARY_NAME))
            .ok_or_else(|| {
                "could not find zust-lsp. Build it with `cargo build -p zust-lsp` or set `lsp.zust-lsp.binary.path` in Zed settings".to_string()
            })?;

        let args = binary_settings
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();

        let mut env = worktree.shell_env();
        if let Some(extra_env) = binary_settings.and_then(|binary| binary.env) {
            env.extend(extra_env);
        }

        Ok(zed::Command { command, args, env })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        if language_server_id.as_ref() != Self::LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(self.language_server_settings(worktree)?.initialization_options)
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<zed::serde_json::Value>> {
        if language_server_id.as_ref() != Self::LANGUAGE_SERVER_ID {
            return Ok(None);
        }

        Ok(self.language_server_settings(worktree)?.settings)
    }
}

zed::register_extension!(ZustExtension);
