//! Workshop overlay: the engine's tool calls as the pager's own tool rows, with the work inside
//! them once the result lands — the diff an edit applied, the output and exit code of a command,
//! the text a read returned — so a row can be opened (`Enter`, fold keys) like any ACP tool row.
//!
//! Shapes come from `opencode serve` 1.18.31 (verified live): `bash` finishes with
//! `metadata.exit` and its combined output; `edit` with `metadata.diff` (a unified diff, also as
//! `metadata.filediff.patch`); `write` with the new content only in its input.

use serde_json::Value;
use xai_grok_pager_diff::{DiffHunk, diff_hunks_from_strings};

use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::{
    EditToolCallBlock, ExecuteToolCallBlock, ListDirToolCallBlock, OtherToolCallBlock,
    ReadToolCallBlock, ToolCallBlock,
};

/// Longest command output kept in a row (the tail); the model saw the whole thing anyway.
const OUTPUT_TAIL_BYTES: usize = 16 * 1024;

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The row's one-line summary: the path, command, pattern or URL of the call.
pub fn summary(input: &Value) -> String {
    crate::app::workshop::summarize_tool_input(input)
}

/// The turn-status row's activity while the call runs, in the shape the ACP tracker reports for a
/// shell turn's tool. A command is the title and the model's `description` (when it gave one) is
/// preferred by the row, as upstream does: `Install Ghostty… 42s`, else `Run sudo apt install …`.
/// The web tools use the tracker's `Web search:` / `Fetch:` titles. File tools carry a description
/// in the transcript row's own words (`Writing hello.txt…`, `Reading …`), since `Run <path>` would
/// read as executing the file.
pub fn turn_activity(name: &str, input: &Value) -> crate::acp::tracker::TurnActivity {
    use crate::acp::tracker::clamp_activity_subject;
    let subject = summary(input);
    let described = |verb: &str| {
        if subject.is_empty() {
            None
        } else {
            Some(clamp_activity_subject(&format!("{verb} {subject}")))
        }
    };
    let (title, description) = match name {
        "bash" | "shell" | "execute" | "run_terminal_command" | "run_terminal_cmd" => (
            if subject.is_empty() {
                name.to_owned()
            } else {
                subject.clone()
            },
            str_of(input, "description")
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(clamp_activity_subject),
        ),
        "websearch" | "web_search" => (
            format!(
                "Web search: {}",
                str_of(input, "query").unwrap_or(subject.as_str())
            ),
            None,
        ),
        "webfetch" | "web_fetch" | "fetch" => (
            format!(
                "Fetch: {}",
                str_of(input, "url").unwrap_or(subject.as_str())
            ),
            None,
        ),
        "write" => (subject.clone(), described("Writing")),
        "edit" | "patch" | "apply_patch" | "search_replace" | "strreplace" => {
            (subject.clone(), described("Editing"))
        }
        "read" | "read_file" => (subject.clone(), described("Reading")),
        "list" | "ls" | "list_dir" => (subject.clone(), described("Listing")),
        "glob" | "grep" | "search" => (subject.clone(), described("Searching")),
        "todowrite" | "todoread" => (name.to_owned(), Some("Updating the plan".to_owned())),
        _ if subject.is_empty() => (name.to_owned(), None),
        _ => (subject.clone(), None),
    };
    crate::acp::tracker::TurnActivity::ToolRunning { title, description }
}

/// The row shown while the call runs: the pager's own verb rows (`◆ Run`, `◆ Edit`,
/// `◆ Creating`, `◈ Read`, …) so engine turns read like shell turns.
pub fn running_row(name: &str, input: &Value) -> RenderBlock {
    let summary = summary(input);
    match name {
        "list" => RenderBlock::list_dir(summary),
        _ => RenderBlock::tool_call(name, summary, true),
    }
}

/// The row once the call finished, with its body: what the user opens to audit the work.
pub fn finished_row(
    name: &str,
    input: &Value,
    ok: bool,
    output: &str,
    title: Option<&str>,
    metadata: &Value,
) -> RenderBlock {
    let summary = summary(input);
    let error = (!ok).then(|| {
        let first = output.lines().next().unwrap_or("Tool call failed").trim();
        if first.is_empty() {
            "Tool call failed".to_owned()
        } else {
            first.to_owned()
        }
    });
    match name {
        "bash" => {
            let command = str_of(input, "command")
                .or(title)
                .unwrap_or(summary.as_str())
                .to_owned();
            let mut block = ExecuteToolCallBlock::new(command);
            if let Some(desc) = str_of(input, "description").filter(|d| !d.trim().is_empty()) {
                block = block.with_description(desc);
            }
            let body = str_of(metadata, "output").unwrap_or(output);
            let body = tail(body);
            if !body.trim().is_empty() && body.trim() != "(no output)" {
                block = block.with_output(body);
            }
            let exit = metadata.get("exit").and_then(Value::as_i64);
            match (exit, error) {
                (_, Some(e)) => block = block.with_error(e),
                (Some(code), None) if code != 0 => block = block.with_error(format!("exit {code}")),
                _ => {}
            }
            RenderBlock::ToolCall(ToolCallBlock::Execute(block))
        }
        "edit" | "write" | "patch" => {
            let path = str_of(input, "filePath")
                .or_else(|| str_of(metadata, "filepath"))
                .or(title)
                .unwrap_or(summary.as_str())
                .to_owned();
            let unified = str_of(metadata, "diff")
                .or_else(|| metadata.get("filediff").and_then(|f| str_of(f, "patch")))
                .filter(|d| !d.trim().is_empty());
            let hunks = match unified {
                Some(diff) => hunks_from_unified_diff(diff),
                None if name == "write" => {
                    let content = str_of(input, "content").unwrap_or_default();
                    diff_hunks_from_strings("", content, 1)
                }
                None => match (str_of(input, "oldString"), str_of(input, "newString")) {
                    (Some(old), Some(new)) => diff_hunks_from_strings(old, new, 1),
                    _ => Vec::new(),
                },
            };
            let mut block = EditToolCallBlock::new(path, hunks);
            if name == "write" && metadata.get("exists").and_then(Value::as_bool) != Some(true) {
                block = block.with_prefix("Creating ");
            }
            if let Some(e) = error {
                block = block.with_error(e);
            }
            RenderBlock::ToolCall(ToolCallBlock::Edit(block))
        }
        "read" => {
            let mut block = ReadToolCallBlock::new(summary);
            if let Some(e) = error {
                block = block.with_error(e);
            }
            RenderBlock::ToolCall(ToolCallBlock::Read(block))
        }
        "list" => {
            let mut block = ListDirToolCallBlock::new(summary).with_output(tail(output));
            if let Some(e) = error {
                block = block.with_error(e);
            }
            RenderBlock::ToolCall(ToolCallBlock::ListDir(block))
        }
        "glob" | "grep" | "webfetch" | "websearch" => {
            // Keep the pager's own verb row for these; the result text is the body.
            let mut block = match RenderBlock::tool_call(name, summary.clone(), true) {
                RenderBlock::ToolCall(tc) => tc,
                other => return other,
            };
            if !ok {
                tc_set_error(&mut block, error.as_deref());
            }
            RenderBlock::ToolCall(block)
        }
        _ => {
            let mut block = OtherToolCallBlock::new(name, summary).with_output(tail(output));
            if let Some(e) = error {
                block = block.with_error(e);
            }
            RenderBlock::ToolCall(ToolCallBlock::Other(block))
        }
    }
}

fn tc_set_error(tc: &mut ToolCallBlock, error: Option<&str>) {
    let error = error.map(str::to_owned);
    match tc {
        ToolCallBlock::Search(b) => b.set_error(error),
        ToolCallBlock::WebFetch(b) => b.set_error(error),
        ToolCallBlock::WebSearch(b) => b.set_error(error),
        ToolCallBlock::Other(b) => b.set_error(error),
        _ => {}
    }
}

/// The last [`OUTPUT_TAIL_BYTES`] of `text`, cut at a line boundary.
fn tail(text: &str) -> String {
    if text.len() <= OUTPUT_TAIL_BYTES {
        return text.to_owned();
    }
    let mut start = text.len() - OUTPUT_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = text.get(start..).unwrap_or_default();
    match cut.find('\n') {
        Some(nl) => format!("…\n{}", cut.get(nl + 1..).unwrap_or_default()),
        None => format!("…{cut}"),
    }
}

/// Parse a unified diff (as `opencode serve` reports it for `edit`: an `Index:` header, `---`/`+++`
/// lines, `@@ -a,b +c,d @@` hunks, `\ No newline at end of file` markers) into the pager's hunks,
/// with old/new line numbers taken from each hunk header.
pub fn hunks_from_unified_diff(diff: &str) -> Vec<DiffHunk> {
    struct Raw {
        old_start: usize,
        old: String,
        new: String,
    }
    let mut raws: Vec<Raw> = Vec::new();
    for line in diff.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if let Some(header) = bare.strip_prefix("@@ ") {
            let old_start = header
                .split_whitespace()
                .next()
                .and_then(|range| range.strip_prefix('-'))
                .and_then(|range| range.split(',').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            raws.push(Raw {
                old_start,
                old: String::new(),
                new: String::new(),
            });
            continue;
        }
        let Some(raw) = raws.last_mut() else {
            continue; // file headers before the first hunk
        };
        if bare.starts_with('\\') {
            // "\ No newline at end of file": the previous line had no trailing newline.
            for side in [&mut raw.old, &mut raw.new] {
                if side.ends_with('\n') {
                    side.pop();
                }
            }
            continue;
        }
        let Some(first) = bare.chars().next() else {
            continue;
        };
        let text = &line[1..];
        match first {
            '-' => raw.old.push_str(text),
            '+' => raw.new.push_str(text),
            ' ' => {
                raw.old.push_str(text);
                raw.new.push_str(text);
            }
            _ => {}
        }
    }
    raws.into_iter()
        .flat_map(|raw| diff_hunks_from_strings(&raw.old, &raw.new, raw.old_start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use similar::ChangeTag;

    const EDIT_DIFF: &str = "Index: /w/hello.txt\n===================================================================\n--- /w/hello.txt\n+++ /w/hello.txt\n@@ -1,1 +1,1 @@\n-hi\n\\ No newline at end of file\n+hello\n\\ No newline at end of file\n";

    #[test]
    fn unified_diff_becomes_one_hunk_with_real_line_numbers() {
        let hunks = hunks_from_unified_diff(EDIT_DIFF);
        assert_eq!(hunks.len(), 1);
        let tags: Vec<(ChangeTag, &str, usize, usize)> = hunks[0]
            .iter()
            .map(|l| (l.tag, l.text.as_str(), l.lo, l.ln))
            .collect();
        assert_eq!(
            tags,
            vec![
                (ChangeTag::Delete, "hi", 1, 1),
                (ChangeTag::Insert, "hello", 2, 1)
            ]
        );
        let later = hunks_from_unified_diff("@@ -40,3 +40,4 @@\n a\n+b\n c\n d\n");
        assert_eq!(later[0][0].lo, 40, "hunk header start is honoured");
        assert!(
            later[0]
                .iter()
                .any(|l| l.tag == ChangeTag::Insert && l.text == "b\n")
        );
        assert!(hunks_from_unified_diff("").is_empty());
    }

    #[test]
    fn turn_activity_is_the_trackers_tool_shape() {
        use crate::acp::tracker::TurnActivity;
        // A command is the title (the row reads `Run <command>`); a description, when the model
        // gave one, is what the row prefers (`{description}…`).
        assert_eq!(
            turn_activity(
                "bash",
                &json!({"command": "sudo apt install ghostty", "description": "Install Ghostty"})
            ),
            TurnActivity::ToolRunning {
                title: "sudo apt install ghostty".into(),
                description: Some("Install Ghostty".into()),
            }
        );
        assert_eq!(
            turn_activity("bash", &json!({"command": "ls", "description": "  "})),
            TurnActivity::ToolRunning {
                title: "ls".into(),
                description: None,
            }
        );
        // File tools describe the step in the row's own words (never `Run <path>`); the web tools
        // use the tracker's `Web search:` / `Fetch:` forms.
        assert_eq!(
            turn_activity(
                "write",
                &json!({"filePath": "/w/hello.txt", "content": "hi"})
            ),
            TurnActivity::ToolRunning {
                title: "/w/hello.txt".into(),
                description: Some("Writing /w/hello.txt".into()),
            }
        );
        assert_eq!(
            turn_activity("read", &json!({"filePath": "/w/a.rs"})),
            TurnActivity::ToolRunning {
                title: "/w/a.rs".into(),
                description: Some("Reading /w/a.rs".into()),
            }
        );
        assert_eq!(
            turn_activity("grep", &json!({"pattern": "TODO"})),
            TurnActivity::ToolRunning {
                title: "TODO".into(),
                description: Some("Searching TODO".into()),
            }
        );
        assert_eq!(
            turn_activity("websearch", &json!({"query": "ghostty ubuntu"})),
            TurnActivity::ToolRunning {
                title: "Web search: ghostty ubuntu".into(),
                description: None,
            }
        );
        assert_eq!(
            turn_activity("webfetch", &json!({"url": "https://example.org"})),
            TurnActivity::ToolRunning {
                title: "Fetch: https://example.org".into(),
                description: None,
            }
        );
        // A tool with nothing to name falls back to its name; a long description is clamped the
        // way the tracker clamps its own.
        assert_eq!(
            turn_activity("task", &json!({})),
            TurnActivity::ToolRunning {
                title: "task".into(),
                description: None,
            }
        );
        let TurnActivity::ToolRunning { description, .. } = turn_activity(
            "bash",
            &json!({"command": "true", "description": "x".repeat(80)}),
        ) else {
            panic!("tool activity");
        };
        assert_eq!(description.map(|d| d.chars().count()), Some(40));
    }

    #[test]
    fn edit_row_carries_the_diff_and_bash_row_the_output_and_exit() {
        let edit = finished_row(
            "edit",
            &json!({"filePath": "/w/hello.txt", "oldString": "hi", "newString": "hello"}),
            true,
            "Edit applied successfully.",
            Some("hello.txt"),
            &json!({"diff": EDIT_DIFF, "filediff": {"patch": EDIT_DIFF, "additions": 1, "deletions": 1}}),
        );
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = edit else {
            panic!("edit row");
        };
        assert_eq!(edit.path, "/w/hello.txt");
        assert_eq!(edit.hunks.len(), 1);
        assert!(edit.is_success());

        let created = finished_row(
            "write",
            &json!({"filePath": "/w/new.txt", "content": "one\ntwo\n"}),
            true,
            "Wrote file successfully.",
            Some("new.txt"),
            &json!({"exists": false, "filepath": "/w/new.txt"}),
        );
        let RenderBlock::ToolCall(ToolCallBlock::Edit(created)) = created else {
            panic!("write row");
        };
        assert_eq!(created.hunks.len(), 1);
        assert_eq!(
            created.hunks[0]
                .iter()
                .filter(|l| l.tag == ChangeTag::Insert)
                .count(),
            2,
            "a new file is all insertions"
        );

        let run = finished_row(
            "bash",
            &json!({"command": "ls /nope"}),
            true,
            "ls: cannot access '/nope'\n",
            Some("ls /nope"),
            &json!({"output": "ls: cannot access '/nope'\n", "exit": 2}),
        );
        let RenderBlock::ToolCall(ToolCallBlock::Execute(run)) = run else {
            panic!("run row");
        };
        assert_eq!(run.command, "ls /nope");
        assert_eq!(run.output.as_deref(), Some("ls: cannot access '/nope'\n"));
        assert_eq!(run.error.as_deref(), Some("exit 2"));
        assert!(!run.is_success());

        let quiet = finished_row(
            "bash",
            &json!({"command": "rm -rf tmp"}),
            true,
            "(no output)",
            Some("rm -rf tmp"),
            &json!({"output": "(no output)", "exit": 0}),
        );
        let RenderBlock::ToolCall(ToolCallBlock::Execute(quiet)) = quiet else {
            panic!("run row");
        };
        assert!(quiet.output.is_none() && quiet.is_success());
    }

    #[test]
    fn running_rows_use_the_pager_verbs() {
        assert!(matches!(
            running_row("bash", &json!({"command": "ls"})),
            RenderBlock::ToolCall(ToolCallBlock::Execute(_))
        ));
        assert!(matches!(
            running_row("write", &json!({"filePath": "a"})),
            RenderBlock::ToolCall(ToolCallBlock::Edit(_))
        ));
        assert!(matches!(
            running_row("read", &json!({"filePath": "a"})),
            RenderBlock::ToolCall(ToolCallBlock::Read(_))
        ));
        assert!(matches!(
            running_row("list", &json!({"path": "/w"})),
            RenderBlock::ToolCall(ToolCallBlock::ListDir(_))
        ));
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let long: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let t = tail(&long);
        assert!(t.starts_with("…\nline "));
        assert!(t.ends_with("line 4999\n"));
        assert!(t.len() <= OUTPUT_TAIL_BYTES + 8);
    }
}
