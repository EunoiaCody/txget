# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**txget** is a Rust CLI tool that extracts questions and answers from `questionData.js` files exported by 天学网 (a Chinese educational platform). It parses JavaScript files containing JSON question data, cleans HTML, classifies question types, and renders structured Markdown output (with optional PDF generation).

## Build & Run Commands

```bash
cargo build                          # Debug build
cargo build --release                # Release build
cargo install --path .               # Install to PATH
cargo test                           # Run all tests (inline in main.rs)
cargo test <test_fn_name>            # Run a single test
cargo test clean_html                # Run tests matching substring
```

No Makefile or task runner — everything goes through Cargo.

## Architecture

**Two source files, one build script:**

- `src/lib.rs` — `Args` struct with clap derive macros (all CLI flags). This is `include!`d by `build.rs`.
- `src/main.rs` — All application logic (~1200 lines): parsing, extraction, classification, rendering, PDF conversion, and inline tests.
- `build.rs` — Generates Fish shell completions at build time and attempts to auto-install them to `~/.config/fish/completions/`.

**Data flow:**
1. **Input detection** (`main`) — directory walk, `.zip`, or `.7z` archive → tempdir extraction
2. **Parsing** (`parse_page_config` → `extract_question_nodes` → `iter_answer_candidates`) — Extract JSON from `var pageConfig = ...;` in JS files, locate question objects
3. **Classification** — Heuristics categorize questions as read-aloud, retelling, or Q&A (`looks_like_read_aloud`, `looks_like_retelling`, `looks_like_qa`)
4. **Post-processing** — `fix_retelling_swap` corrects known data issue where retelling question/answer text is swapped
5. **Rendering** (`process_and_write_group`) — Groups by extracted set name, sorts by question order, writes Markdown
6. **PDF** (`convert_md_to_pdf`) — Optional `--pdf` flag triggers pulldown-cmark → genpdf pipeline; searches system for CJK fonts (LXGWWenKai, NotoSansCJK)

**Key data structure:** `Entry` holds question_id, text, answers, analysis, source_file, and type metadata. All classification logic works on this struct.

## Testing

All tests are inline in `main.rs` as a `#[cfg(test)] mod tests` block (lines ~877–1193). ~30 test functions covering HTML cleaning, parsing, classification heuristics, and filename sanitization. No separate test files or integration tests. Notably untested: PDF generation (`convert_md_to_pdf`), archive extraction paths, and `main()` end-to-end.

## Notable Details

- **Rust edition 2024** — uses newer Rust idioms and features.
- **Security** — `safe_extract_sevenz` implements path-traversal protection for 7z extraction (Zip-Slip mitigation).
- **Fish completions** — `build.rs` auto-installs to `~/.config/fish/completions/` on every build.
- **Regex patterns** — `QA_RE` and `ZH_NUM_RE` handle English/Chinese question numbering for sorting.
- **The `--pdf` flag is recent** — not yet documented in README.md. `--font-dir` allows specifying CJK font directory.