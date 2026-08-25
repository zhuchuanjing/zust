//! `patch::*` —— 上下文锚定的文本补丁（Codex apply_patch 语义的纯函数子集）。
//!
//! - `patch::apply(content, diff) -> map` 在字符串上应用补丁，返回
//!   `{ok: true, content}` 或 `{ok: false, error}`。
//! - diff 由若干 hunk 组成，`@@` 或 `***` 开头的行是 hunk 边界；hunk 内
//!   `-` 前缀是删除行、`+` 前缀是新增行、其余（含空行）是上下文行，
//!   上下文行允许带一个 diff 风格的前导空格。
//! - old 侧（上下文+删除行）必须按原顺序在 content 中找到匹配，替换为
//!   new 侧（上下文+新增行）；任一 hunk 匹配失败则整体失败，内容不变。
//!   纯函数不落盘——"校验不过不落盘"由它天然保证，写文件是调用方的事。
//!
//! 主要消费方是 agent 沙箱 VM：模型用 process::run 改文件的替代路径，
//! 补丁不匹配时模型拿到明确的错误而不是半改的状态。
use crate::memory::alloc_dynamic;
use dynamic::{Dynamic, Type};

extern "C" fn patch_apply(content: *const Dynamic, diff: *const Dynamic) -> *const Dynamic {
    let content = unsafe { (&*content).clone() };
    let diff = unsafe { (&*diff).clone() };
    alloc_dynamic(apply(&content, &diff))
}

fn apply(content: &Dynamic, diff: &Dynamic) -> Dynamic {
    let (Some(content), Some(diff)) = (as_text(content), as_text(diff)) else {
        return error_result("content and diff must be string");
    };
    let hunks = parse_hunks(&diff);
    if hunks.is_empty() {
        return error_result("diff contains no hunks");
    }

    let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
    for hunk in &hunks {
        if let Err(err) = replace_once(&mut lines, hunk) {
            return error_result(&err);
        }
    }
    dynamic::map!("ok" => true, "content" => lines.join("\n"))
}

struct Hunk {
    old: Vec<String>,
    new: Vec<String>,
}

/// hunk 边界行（`@@`/`***`）之间的行构成一个 hunk；old/new 侧交错保持
/// 出现顺序，上下文行同时进入两侧。
fn parse_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;
    for line in diff.lines() {
        if line.starts_with("***") || line.starts_with("@@") {
            flush(&mut hunks, &mut current);
            continue;
        }
        let hunk = current.get_or_insert_with(|| Hunk { old: Vec::new(), new: Vec::new() });
        if let Some(rest) = line.strip_prefix('-') {
            hunk.old.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('+') {
            hunk.new.push(rest.to_string());
        } else {
            let context = line.strip_prefix(' ').unwrap_or(line);
            hunk.old.push(context.to_string());
            hunk.new.push(context.to_string());
        }
    }
    flush(&mut hunks, &mut current);
    hunks
}

fn flush(hunks: &mut Vec<Hunk>, current: &mut Option<Hunk>) {
    if let Some(hunk) = current.take() {
        if !hunk.old.is_empty() || !hunk.new.is_empty() {
            hunks.push(hunk);
        }
    }
}

/// old 侧为空（纯新增无锚点）拒绝执行：没有上下文的补丁无法校验位置。
fn replace_once(lines: &mut Vec<String>, hunk: &Hunk) -> Result<(), String> {
    if hunk.old.is_empty() {
        return Err("hunk has no context lines (old side empty)".to_string());
    }
    let anchor = hunk.old.first().map(String::as_str).unwrap_or_default();
    if lines.len() < hunk.old.len() {
        return Err(format!("context mismatch: file has {} lines, hunk needs {}", lines.len(), hunk.old.len()));
    }
    for start in 0..=(lines.len() - hunk.old.len()) {
        if lines[start..start + hunk.old.len()] == hunk.old[..] {
            let end = start + hunk.old.len();
            lines.splice(start..end, hunk.new.iter().cloned());
            return Ok(());
        }
    }
    Err(format!("context mismatch: hunk starting with {anchor:?} not found"))
}

fn error_result(message: &str) -> Dynamic {
    dynamic::map!("ok" => false, "error" => message)
}

fn as_text(value: &Dynamic) -> Option<String> {
    match value {
        Dynamic::String(text) => Some(text.to_string()),
        Dynamic::StringBuf(text) => Some(text.clone()),
        _ => None,
    }
}

pub const PATCH_NATIVE: [(&str, &[Type], Type, *const u8); 1] =
    [("apply", &[Type::Any, Type::Any], Type::Any, patch_apply as *const u8)];

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_str(content: &str, diff: &str) -> Dynamic {
        apply(&Dynamic::from(content), &Dynamic::from(diff))
    }

    fn field(value: &Dynamic, key: &str) -> Dynamic {
        if let Dynamic::Map(map) = value {
            map.read().get(key).cloned().unwrap_or(Dynamic::Null)
        } else {
            Dynamic::Null
        }
    }

    #[test]
    fn replaces_marked_line() {
        let diff = "@@\n let a = 1;\n-let b = 2;\n+let b = 3;\n let c = 4;";
        let result = apply_str("let a = 1;\nlet b = 2;\nlet c = 4;", diff);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(true));
        assert_eq!(field(&result, "content"), Dynamic::from("let a = 1;\nlet b = 3;\nlet c = 4;"));
    }

    #[test]
    fn applies_multiple_hunks_in_order() {
        let diff = "@@\n-a\n+b\n@@\n-x\n+y";
        let result = apply_str("a\nm\nx", diff);
        assert_eq!(field(&result, "content"), Dynamic::from("b\nm\ny"));
    }

    #[test]
    fn insert_lines_with_context_anchor() {
        let diff = "@@\n start\n+added\n end";
        let result = apply_str("start\nend", diff);
        assert_eq!(field(&result, "content"), Dynamic::from("start\nadded\nend"));
    }

    #[test]
    fn delete_lines() {
        let diff = "@@\n keep\n-drop me\n tail";
        let result = apply_str("keep\ndrop me\ntail", diff);
        assert_eq!(field(&result, "content"), Dynamic::from("keep\ntail"));
    }

    #[test]
    fn context_mismatch_fails_without_content() {
        let diff = "@@\n-nope\n+yes";
        let result = apply_str("a\nb", diff);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
        assert!(as_text(&field(&result, "error")).unwrap().contains("context mismatch"));
    }

    #[test]
    fn empty_diff_fails() {
        let result = apply_str("abc", "@@\n@@");
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
    }

    #[test]
    fn bare_addition_without_context_rejected() {
        let diff = "@@\n+lonely";
        let result = apply_str("a", diff);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
    }

    #[test]
    fn blank_line_is_context() {
        let diff = "@@\n a\n\n b\n+new";
        let result = apply_str("a\n\nb", diff);
        assert_eq!(field(&result, "content"), Dynamic::from("a\n\nb\nnew"));
    }

    #[test]
    fn codex_style_markers_ignored() {
        let diff = "*** Begin Patch\n*** Update File: x\n@@\n ctx\n-old\n+new\n*** End Patch";
        let result = apply_str("ctx\nold", diff);
        assert_eq!(field(&result, "content"), Dynamic::from("ctx\nnew"));
    }
}
