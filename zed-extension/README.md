# Zust for Zed

这个目录是一个可直接安装到 Zed 的 dev extension，用来把仓库里的 `zust-lsp` 接进 Zed，并附带 Zust 的 tree-sitter grammar。

## 当前能力

- `*.zs` 文件识别
- `zust-lsp` 提供的诊断、补全、hover、跳转、文档符号
- 本地 `tree-sitter-zust` 语法高亮与结构查询

## 安装方式

1. 先在当前仓库构建 LSP：

   ```bash
   rustup target add wasm32-wasip2
   cargo build -p zust-lsp
   ```

2. 打开 Zed，执行 `zed: install dev extension`
3. 选择仓库里的 `zed-extension` 目录。

## 二进制查找顺序

扩展启动 `zust-lsp` 时按下面顺序查找：

1. `lsp.zust-lsp.binary.path`
2. 当前工作区的 `target/debug/zust-lsp`
3. 当前工作区所在 Git 仓库根目录的 `target/debug/zust-lsp`
4. `PATH` 里的 `zust-lsp`

如果你在 Zed 里只打开了仓库子目录，例如 `zusts/`，第 3 步会通过 `git rev-parse --show-toplevel` 找回仓库根目录。

如果你想显式指定路径，可以在 Zed 设置里写：

```json
{
  "lsp": {
    "zust-lsp": {
      "binary": {
        "path": "/path/to/zust/target/debug/zust-lsp"
      }
    }
  }
}
```

## 可选配置

`zust-lsp` 的初始化参数和 workspace settings 会透传自：

```json
{
  "lsp": {
    "zust-lsp": {
      "initialization_options": {},
      "settings": {}
    }
  }
}
```

## 已知限制

- Zed 要求 `extension.toml` 中的 grammar 通过 Git repository 和 revision 注册。当前配置从公开的 Zust 仓库拉取固定 revision，并通过 `path = "zed-extension/tree-sitter-zust"` 指向内置 grammar 目录。
- 这一版 grammar 已覆盖 Zust 当前常见语法：函数、结构体、`impl`、闭包、list/map、结构体初始化、路径调用、range、类型标注和基础表达式。后面如果你要把 outline、folding、runnables 做细，还可以继续补对应的 query 文件。

## 排查

如果扩展安装成功但 Select Language 菜单里没有 `Zust`，通常是语言加载阶段的 query 报错。打开 `zed: open log`，搜索 `failed to load language Zust`，日志会说明是 `highlights.scm`、`brackets.scm` 还是其他 query 文件的问题。
