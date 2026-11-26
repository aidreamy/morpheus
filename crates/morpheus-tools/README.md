# morpheus-tools

[![Crates.io](https://img.shields.io/crates/v/morpheus-tools.svg)](https://crates.io/crates/morpheus-tools)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/aidreamy/morpheus/blob/main/LICENSE)

Tools crate for [Morpheus](https://github.com/aidreamy/morpheus), a coding agent for the terminal.

`morpheus-tools` implements every tool the Morpheus agent harness exposes to the LLM: filesystem operations, bash execution, search, LSP, web, desktop control, skills, planning and structured user questions. Each tool is a self-contained async builder API plus an [MCP-schema](https://modelcontextprotocol.io/specification/2025-11-25/server/tools) tool description, so the crate can be used standalone in your own agent with the same tools the Morpheus app registers.

## Highlights

- **Sandboxed file editing**: reads carry a `file_hash` that writes must verify; edits are anchored on hashline tags with per-target errors, plus fuzzy-equivalent matching and AST-based structural rewrites (`ast-grep` and `tree-sitter`). Every write records a version for session rollback.
- **Path guarding**: all paths are denied until the project root is set; write and read-only allow/blocklists, and kernel-pinned sandboxing (`openat` with `O_NOFOLLOW`) on Unix.
- **Accessibility-tree-first computer tools**: observe and drive native desktop apps through the OS accessibility tree ([`xa11y`](https://crates.io/crates/xa11y)), with screenshots and synthetic input as support. Works on Windows (UIA), macOS, and Linux (X11 and Wayland).
- **Streaming bash**: stdout/stderr streamed as chunks with exit codes, PTY mode, timeouts and pre-spawn security validation.
- **LSP integration**: definitions, references, renames, call hierarchy and passive diagnostics feedback on touched files.
- **Skills system**: schema-driven `SKILL.md` capability packs with glob matching and sandboxed asset reads.
- **Vector recall** (optional, `embed` feature): embedding-agnostic LanceDB similarity search.

## Tools

| Module             | Tools                                                                                                                                |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `fs`               | `fs_read`, `fs_write`, `fs_edit`, `fs_edit_lines`, `fs_ast_edit`, `fs_rollback`                                                      |
| `bash`             | `bash_run`                                                                                                                           |
| `find`             | `find_glob`, `find_grep`                                                                                                             |
| `lsp`              | `lsp_definitions`, `lsp_references`, `lsp_symbols`, `lsp_rename`, `lsp_hover`, `lsp_code_actions`, …                                 |
| `computer`         | `computer_apps`, `computer_snapshot`, `computer_act`, `computer_control`, `computer_screenshot`, `computer_wait`, `computer_windows` |
| `web`              | `web_fetch`, `web_search`                                                                                                            |
| `skills`           | `skills_list`, `skills_read`, `skills_read_asset`, `skills_match_skills`                                                             |
| `plan`             | `plan_todo_write`                                                                                                                    |
| `question`         | `ask_questions`                                                                                                                      |
| `recall` (`embed`) | `recall_search`                                                                                                                      |

## Usage

```rust
use morpheus_tools::fs::{Fs, Target};

#[tokio::main]
async fn main() {
    let fs = Fs::new().cwd("/path/to/project");

    // Read a file. The result carries a file_hash and a hashline header.
    let results = fs
        .read(vec![Target {
            path: "src/main.rs".to_string(),
            line: None,
            symbol: None,
            line_range: None,
            offset: None,
            limit: None,
        }])
        .await;

    println!("file_hash: {}", results[0].file_hash);
    println!("{}", results[0].content);
}
```

More examples in [`examples/`](https://github.com/aidreamy/morpheus/tree/main/crates/morpheus-tools/examples); run them with `cargo run --example fs-read` (also `bash`, `computer-control`, and others).

## Requirements

- Rust 2024 edition (≥ 1.85)
- The `computer` module needs an OS accessibility stack: Windows UIA, macOS AX, or Linux X11/Wayland

## Related crates

- [`morpheus-agents`](https://crates.io/crates/morpheus-agents): ACP sub-agent integration
- [`morpheus-sdk`](https://crates.io/crates/morpheus-sdk): agent harness SDK
- [`morpheus-recall`](https://crates.io/crates/morpheus-recall): memory and context recall
- [`morpheus-tui`](https://crates.io/crates/morpheus-tui): terminal interface

## License

Apache-2.0. See [LICENSE](https://github.com/aidreamy/morpheus/blob/main/LICENSE).
