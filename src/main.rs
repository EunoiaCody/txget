use anyhow::{Context, Result};
use clap::Parser as ClapParser;
use genpdf::elements;
use genpdf::fonts::{FontData, FontFamily};
use html_escape::decode_html_entities;
use pulldown_cmark::{Event, HeadingLevel, Parser as MarkdownParser, Tag, TagEnd};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tempfile::tempdir;
use walkdir::WalkDir;
use zip::ZipArchive;
// Note: sevenz_rust is used indirectly via safe_extract_sevenz which calls
// decompress_with_extract_fn with path-traversal protection.

use txget::Args;

/// Safely extract a .7z archive, rejecting any entry whose path escapes the
/// destination directory (Zip-Slip / path-traversal mitigation).
fn safe_extract_sevenz(src: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(src)
        .with_context(|| format!("Failed to open 7z archive: {}", src.display()))?;
    let dest = dest.to_path_buf();
    let dest_for_cb = dest.clone();
    sevenz_rust::decompress_with_extract_fn(file, &dest, move |entry, reader, out_path| {
        // Validate: out_path must stay inside dest
        if out_path.is_absolute() {
            return Err(sevenz_rust::Error::other(format!(
                "Path traversal: absolute path in archive: {}",
                entry.name()
            )));
        }
        match out_path.strip_prefix(&dest_for_cb) {
            Ok(rel) => {
                if rel.starts_with("..") || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                    return Err(sevenz_rust::Error::other(format!(
                        "Path traversal: path escapes destination: {}",
                        entry.name()
                    )));
                }
            }
            Err(_) => {
                // out_path is not under dest at all
                return Err(sevenz_rust::Error::other(format!(
                    "Path traversal: path outside destination: {}",
                    entry.name()
                )));
            }
        }

        if entry.is_directory() {
            std::fs::create_dir_all(out_path).map_err(|e| sevenz_rust::Error::io_msg(e, "create_dir"))?;
        } else {
            if let Some(parent) = out_path.parent()
            && !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| sevenz_rust::Error::io_msg(e, "create_dir"))?;
            }
            let mut outfile = std::fs::File::create(out_path)
                .map_err(|e| sevenz_rust::Error::io_msg(e, "file_create"))?;
            if entry.size() > 0 {
                std::io::copy(reader, &mut outfile).map_err(sevenz_rust::Error::io)?;
            }
        }
        Ok(true)
    })?;
    Ok(())
}

static BR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<\s*br\s*/?\s*>").unwrap());
static BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)</\s*(p|div|li|h[1-6])\s*>").unwrap());
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static QA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)get ready to answer the (first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth) question").unwrap());
static ZH_NUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第([一二三四五六七八九十])个问题").unwrap());

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Entry {
    question_id: String,
    question_text: String,
    answers: Vec<String>,
    analysis: String,
    source_file: String,
    question_type: Option<serde_json::Value>,
    qtype_id: Option<serde_json::Value>,
}

fn clean_html_text(text: Option<&serde_json::Value>) -> String {
    let s = match text {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => return String::new(),
    };

    let s = BR_RE.replace_all(&s, "\n");
    let s = BLOCK_RE.replace_all(&s, "\n");
    let s = TAG_RE.replace_all(&s, "");
    let s = decode_html_entities(&s);

    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_page_config(raw: &str) -> Result<serde_json::Value> {
    let re = Regex::new(r"(?s)var\s+pageConfig\s*=\s*(\{.*\})\s*;?\s*$").unwrap();
    if let Some(caps) = re.captures(raw.trim()) {
        return serde_json::from_str(&caps[1])
            .map_err(|e| anyhow::anyhow!("JSON parse error: {}", e));
    }

    let start = raw
        .find('{')
        .context("Cannot locate pageConfig JSON object (start)")?;
    let end = raw
        .rfind('}')
        .context("Cannot locate pageConfig JSON object (end)")?;
    serde_json::from_str(&raw[start..end + 1])
        .map_err(|e| anyhow::anyhow!("JSON parse error: {}", e))
}

fn extract_question_nodes(page_config: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut nodes = Vec::new();
    if let Some(qobj) = page_config.get("questionObj") {
        if qobj.is_object() {
            nodes.push(qobj.clone());
        }
    }

    if let Some(sliders) = page_config.get("sliders").and_then(|s| s.as_array()) {
        for slider in sliders {
            if let Some(qlist) = slider.get("questionList").and_then(|ql| ql.as_array()) {
                for q in qlist {
                    if q.is_object() {
                        nodes.push(q.clone());
                    }
                }
            }
        }
    }
    nodes
}

fn iter_answer_candidates(question: &serde_json::Value) -> Vec<String> {
    let mut candidates = Vec::new();

    if let Some(record_speak) = question.get("record_speak").and_then(|r| r.as_array()) {
        for item in record_speak {
            let content = clean_html_text(item.get("content"));
            if !content.is_empty() {
                candidates.push(content);
            }
        }
    }

    if let Some(options) = question.get("options").and_then(|o| o.as_array()) {
        for opt in options {
            let txt = if let Some(s) = opt.as_str() {
                clean_html_text(Some(&serde_json::Value::String(s.to_string())))
            } else if opt.is_object() {
                let content = opt
                    .get("content")
                    .or_else(|| opt.get("text"))
                    .or_else(|| opt.get("title"))
                    .or_else(|| opt.get("value"));
                clean_html_text(content)
            } else {
                String::new()
            };
            if !txt.is_empty() {
                candidates.push(txt);
            }
        }
    }

    let answer_text = clean_html_text(question.get("answer_text"));
    if !answer_text.is_empty() && answer_text != "<answers/>" && answer_text != "answers/" {
        candidates.push(answer_text);
    }

    candidates
}

fn select_shortest_answers(answers: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut uniq = Vec::new();
    for a in answers {
        if seen.insert(a.clone()) {
            uniq.push(a);
        }
    }

    let mut indexed: Vec<(usize, String)> = uniq.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| a.1.len().cmp(&b.1.len()).then_with(|| a.0.cmp(&b.0)));
    indexed.into_iter().take(limit).map(|(_, s)| s).collect()
}

fn process_file(path: &Path) -> Result<Vec<Entry>> {
    let raw = fs::read_to_string(path)?;
    let page_config = parse_page_config(&raw)?;
    let questions = extract_question_nodes(&page_config);
    let mut entries = Vec::new();

    for q in questions {
        let qid = q
            .get("question_id")
            .and_then(|id| {
                if id.is_string() {
                    id.as_str().map(|s| s.to_string())
                } else {
                    Some(id.to_string())
                }
            })
            .unwrap_or_else(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });

        let question_text = clean_html_text(q.get("question_text"));
        let analysis = clean_html_text(q.get("analysis"));
        let raw_answers = iter_answer_candidates(&q);
        let answers = select_shortest_answers(raw_answers, 5);

        entries.push(Entry {
            question_id: qid,
            question_text,
            answers,
            analysis,
            source_file: path.to_string_lossy().to_string(),
            question_type: q.get("question_type").cloned(),
            qtype_id: q.get("qtype_id").cloned(),
        });
    }
    Ok(entries)
}

fn looks_like_read_aloud(e: &Entry) -> bool {
    if !e.answers.is_empty() {
        return false;
    }
    let q = &e.question_text;
    let has_english = q.chars().any(|c| c.is_ascii_alphabetic());
    let english_chars = q.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let chinese_chars = q
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .count();
    let long_text = q.len() >= 180;
    let mainly_english = english_chars > std::cmp::max(30, (chinese_chars as f64 * 3.0) as usize);
    has_english && long_text && mainly_english
}

fn looks_like_retelling(e: &Entry) -> bool {
    let q = e.question_text.to_lowercase();
    let a = e.analysis.to_lowercase();
    let max_ans_len = e.answers.iter().map(|s| s.len()).max().unwrap_or(0);
    let has_retell_hint = q.contains("梗概")
        || q.contains("关键词")
        || q.contains("复述")
        || q.contains("retelling")
        || q.contains("retell")
        || a.contains("参考复述")
        || a.contains("复述");
    has_retell_hint && max_ans_len >= 120
}

fn fix_retelling_swap(e: &Entry) -> (String, Vec<String>) {
    if !looks_like_retelling(e) {
        return (e.question_text.clone(), e.answers.clone());
    }

    let all_candidates: Vec<&String> = e.answers.iter().chain(std::iter::once(&e.question_text)).collect();

    let mut shortest: Option<&String> = None;
    let mut second_longest: Option<&String> = None;
    let mut longest: Option<&String> = None;
    let mut longest_len = 0;
    let mut second_len = 0;
    let mut shortest_len = usize::MAX;

    for c in &all_candidates {
        let len = c.len();
        if len > longest_len {
            second_len = longest_len;
            second_longest = longest.clone();
            longest_len = len;
            longest = Some(*c);
        } else if len > second_len {
            second_len = len;
            second_longest = Some(*c);
        }
        if len < shortest_len {
            shortest_len = len;
            shortest = Some(*c);
        }
    }

    let longest = longest.as_ref();
    let shortest = shortest.as_ref();

    let use_longest_as_question = longest.map_or(false, |l| {
        shortest.map_or(false, |s| {
            l.len() >= s.len() * 3 && l.len() >= s.len() + 200 && e.question_text.len() < s.len()
        })
    });

    if use_longest_as_question {
        let new_question = (*longest.unwrap()).clone();
        let new_answer = (*shortest.unwrap()).clone();
        let mut new_answers = e.answers.clone();
        new_answers.retain(|a| a != &new_question && a != &new_answer);
        new_answers.insert(0, new_answer);
        if let Some(second) = second_longest {
            if new_answers.len() < 2 && !new_answers.contains(second) {
                new_answers.push((*second).clone());
            }
        }
        (new_question, new_answers)
    } else {
        (e.question_text.clone(), e.answers.clone())
    }
}

fn extract_qa_order_index(question_text: &str) -> i32 {
    let text = question_text.to_lowercase();
    let en_map = [
        ("first question", 1),
        ("second question", 2),
        ("third question", 3),
        ("fourth question", 4),
        ("fifth question", 5),
        ("sixth question", 6),
        ("seventh question", 7),
        ("eighth question", 8),
        ("ninth question", 9),
        ("tenth question", 10),
    ];
    for (k, v) in en_map {
        if text.contains(k) {
            return v;
        }
    }

    if let Some(caps) = ZH_NUM_RE.captures(question_text) {
        let zh_map = std::collections::HashMap::from([
            ("一", 1),
            ("二", 2),
            ("三", 3),
            ("四", 4),
            ("五", 5),
            ("六", 6),
            ("七", 7),
            ("八", 8),
            ("九", 9),
            ("十", 10),
        ]);
        return *zh_map.get(&caps[1]).unwrap_or(&999);
    }
    999
}

fn looks_like_qa(e: &Entry) -> bool {
    let q = e.question_text.to_lowercase();
    if (q.contains("第") && q.contains("个问题")) || q.contains("question.") {
        return true;
    }
    QA_RE.is_match(&q)
}

fn contains_chinese(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn extract_group_name(path: &Path) -> String {
    // Walk up from questionData.js to find the "questions" directory,
    // then group by the parent of "questions" (the set/book/exam identifier):
    //   .../<set_uuid>/questions/<question_uuid>/questionData.js
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir.file_name().and_then(|s| s.to_str()) == Some("questions") {
            if let Some(set_dir) = dir.parent() {
                if let Some(name) = set_dir.file_name().and_then(|s| s.to_str()) {
                    return name.to_string();
                }
            }
            break;
        }
        current = dir.parent();
    }
    // Fallback: use immediate parent directory name
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn render_entry_with_text(question_text: &str, answers: &[String], e: &Entry, include_analysis: bool, include_source: bool) -> String {
    let mut out = format!("### {}\n", e.question_id);
    if include_source {
        out.push_str(&format!("- 来源：`{}`\n", e.source_file));
    }
    out.push_str(&format!(
        "- 题型：`question_type={}`，`qtype_id={}`\n\n",
        e.question_type.as_ref().unwrap_or(&serde_json::Value::Null),
        e.qtype_id.as_ref().unwrap_or(&serde_json::Value::Null)
    ));
    out.push_str("#### 题目\n");
    out.push_str(if question_text.is_empty() {
        "_（空）_"
    } else {
        question_text
    });
    out.push_str("\n\n#### 参考答案\n");
    if answers.is_empty() {
        out.push_str("_未提取到可见答案_\n");
    } else {
        for (i, a) in answers.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, a));
        }
    }
    if include_analysis && !e.analysis.is_empty() {
        out.push_str("\n#### 解析/线索\n");
        out.push_str(&e.analysis);
        out.push_str("\n");
    }
    out.push_str("\n");
    out
}

fn render_entry(e: &Entry, include_analysis: bool, include_source: bool) -> String {
    render_entry_with_text(&e.question_text, &e.answers, e, include_analysis, include_source)
}

fn process_and_write_group(
    group_name: &str,
    entries: Vec<Entry>,
    out_path: &Path,
    include_analysis: bool,
    include_source: bool,
) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut read_aloud = Vec::new();
    let mut translation = Vec::new();
    let mut qa = Vec::new();
    let mut retelling = Vec::new();
    let mut others = Vec::new();

    for e in &entries {
        if looks_like_read_aloud(e) {
            read_aloud.push(e.clone());
        } else if looks_like_retelling(e) {
            retelling.push(e.clone());
        } else if looks_like_qa(e) {
            qa.push(e.clone());
        } else if contains_chinese(&e.question_text)
            && e.answers
                .iter()
                .any(|a| a.chars().any(|c| c.is_ascii_alphabetic()))
        {
            translation.push(e.clone());
        } else {
            others.push(e.clone());
        }
    }

    for e in retelling.iter_mut() {
        let (qt, ans) = fix_retelling_swap(e);
        e.question_text = qt;
        e.answers = ans;
    }

    qa.sort_by_key(|e| {
        (
            extract_qa_order_index(&e.question_text),
            e.question_id.clone(),
        )
    });
    let sort_by_id = |a: &Entry, b: &Entry| a.question_id.cmp(&b.question_id);
    translation.sort_by(sort_by_id);
    read_aloud.sort_by(sort_by_id);
    retelling.sort_by(sort_by_id);
    others.sort_by(sort_by_id);

    let mut markdown = format!("# 题目与答案提取结果 — {}\n\n", group_name);
    markdown.push_str(&format!("- 题目总数：{}\n", entries.len()));
    markdown.push_str(&format!("- 第一部分（跟随朗读）：{}\n", read_aloud.len()));
    markdown.push_str(&format!("- 第二部分（翻译题）：{}\n", translation.len()));
    markdown.push_str(&format!("- 第三部分（问答题）：{}\n", qa.len()));
    markdown.push_str(&format!("- 第四部分（Retelling）：{}\n", retelling.len()));
    if !others.is_empty() {
        markdown.push_str(&format!("- 其他未归类：{}\n", others.len()));
    }
    markdown.push_str("\n");

    let sections = [
        ("## 第一部分：跟随文章朗读", &read_aloud),
        ("## 第二部分：翻译题（中文题目 -> 英文答案）", &translation),
        ("## 第三部分：问答题（按第几个问题顺序）", &qa),
        ("## 第四部分：Retelling", &retelling),
    ];

    for (title, sec_entries) in sections {
        markdown.push_str(title);
        markdown.push_str("\n\n");
        for e in sec_entries {
            markdown.push_str(&render_entry(e, include_analysis, include_source));
        }
    }

    if !others.is_empty() {
        markdown.push_str("## 其他未归类\n\n");
        for e in &others {
            markdown.push_str(&render_entry(e, include_analysis, include_source));
        }
    }

    fs::write(out_path, markdown)?;
    Ok(())
}

fn convert_md_to_pdf(md_path: &Path, font_dir: Option<&Path>) -> Result<()> {
    let md_content = fs::read_to_string(md_path)?;

    struct FontCandidate {
        regular: &'static str,
        bold: &'static str,
    }

    static CJK_FONT_CANDIDATES: &[FontCandidate] = &[
        FontCandidate {
            regular: "LXGWWenKai-Regular.ttf",
            bold: "LXGWWenKai-Medium.ttf",
        },
        FontCandidate {
            regular: "NotoSansCJK-Regular.ttc",
            bold: "NotoSansCJK-Bold.ttc",
        },
        FontCandidate {
            regular: "NotoSansCJKsc-Regular.ttc",
            bold: "NotoSansCJKsc-Bold.ttc",
        },
    ];

    static SEARCH_DIRS: &[&str] = &[
        "/usr/share/fonts/TTF",
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/noto-cjk",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/noto",
    ];

    let (regular_path, bold_path) = if let Some(dir) = font_dir {
        let dir = Path::new(dir);
        let mut found = None;
        for c in CJK_FONT_CANDIDATES {
            let r = dir.join(c.regular);
            if r.exists() {
                found = Some((r, dir.join(c.bold)));
                break;
            }
        }
        match found {
            Some(paths) => paths,
            None => anyhow::bail!(
                "No CJK font found in {}. Searched for: {}",
                dir.display(),
                CJK_FONT_CANDIDATES
                    .iter()
                    .map(|c| c.regular)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    } else {
        let mut found = None;
        for dir in SEARCH_DIRS {
            let dir_path = Path::new(dir);
            for c in CJK_FONT_CANDIDATES {
                let r = dir_path.join(c.regular);
                if r.exists() {
                    found = Some((r, dir_path.join(c.bold)));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        match found {
            Some(paths) => paths,
            None => anyhow::bail!(
                "No CJK font found. Searched in:\n{}\n\
                 Install with: sudo pacman -S noto-fonts-cjk\n\
                 Or specify a font directory with --font-dir <path>",
                SEARCH_DIRS
                    .iter()
                    .map(|d| format!("  - {}", d))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        }
    };

    let family = FontFamily {
        regular: FontData::load(&regular_path, None)?,
        bold: FontData::load(
            if bold_path.exists() {
                &bold_path
            } else {
                &regular_path
            },
            None,
        )?,
        italic: FontData::load(&regular_path, None)?,
        bold_italic: FontData::load(
            if bold_path.exists() {
                &bold_path
            } else {
                &regular_path
            },
            None,
        )?,
    };

    let mut doc = genpdf::Document::new(family);
    doc.set_minimal_conformance();
    doc.set_font_size(11);

    let parser = MarkdownParser::new(&md_content);
    let mut text_buffer = String::new();
    let mut list_strings: Vec<String> = Vec::new();
    let mut ordered_list = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    if !text_buffer.trim().is_empty() {
                        doc.push(elements::Paragraph::new(
                            std::mem::take(&mut text_buffer).trim().to_string(),
                        ));
                    }
                    let size = match level {
                        HeadingLevel::H1 => 18,
                        HeadingLevel::H2 => 15,
                        HeadingLevel::H3 => 13,
                        HeadingLevel::H4 => 11,
                        _ => 11,
                    };
                    doc.set_font_size(size);
                }
                Tag::List(start) => {
                    ordered_list = start.is_some();
                    list_strings = Vec::new();
                }
                Tag::Item => {
                    text_buffer.clear();
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    if !text_buffer.trim().is_empty() {
                        doc.push(elements::Paragraph::new(
                            std::mem::take(&mut text_buffer).trim().to_string(),
                        ));
                    }
                    doc.set_font_size(11);
                    doc.push(elements::Break::new(1));
                }
                TagEnd::Paragraph => {
                    if !text_buffer.trim().is_empty() {
                        doc.push(elements::Paragraph::new(
                            std::mem::take(&mut text_buffer).trim().to_string(),
                        ));
                    }
                }
                TagEnd::Item => {
                    let text = std::mem::take(&mut text_buffer);
                    if !text.trim().is_empty() {
                        list_strings.push(text.trim().to_string());
                    }
                }
                TagEnd::List(_) => {
                    if !list_strings.is_empty() {
                        if ordered_list {
                            let mut list = elements::OrderedList::new();
                            for s in std::mem::take(&mut list_strings) {
                                list.push(elements::Paragraph::new(s));
                            }
                            doc.push(list);
                        } else {
                            let mut list = elements::UnorderedList::new();
                            for s in std::mem::take(&mut list_strings) {
                                list.push(elements::Paragraph::new(s));
                            }
                            doc.push(list);
                        }
                    }
                    doc.push(elements::Break::new(1));
                }
                _ => {}
            },
            Event::Text(text) => {
                text_buffer.push_str(&text);
            }
            Event::Code(text) => {
                text_buffer.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak => {
                text_buffer.push('\n');
            }
            _ => {}
        }
    }

    if !text_buffer.trim().is_empty() {
        doc.push(elements::Paragraph::new(
            std::mem::take(&mut text_buffer).trim().to_string(),
        ));
    }

    let pdf_path = md_path.with_extension("pdf");
    doc.render_to_file(&pdf_path)?;
    println!("  PDF: {}", pdf_path.display());
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let input_path = Path::new(&args.file);
    let mut grouped: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    let mut output_dir = PathBuf::from(".");
    let mut file_errors: Vec<(String, anyhow::Error)> = Vec::new();

    macro_rules! process_entry {
        ($path:expr) => {
            match process_file($path) {
                Ok(file_entries) => {
                    let group = extract_group_name($path);
                    grouped.entry(group).or_default().extend(file_entries);
                }
                Err(e) => {
                    file_errors.push(($path.display().to_string(), e));
                }
            }
        };
    }

    if input_path.is_file() && input_path.extension().map_or(false, |ext| ext == "zip") {
        output_dir = input_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let dir = tempdir()?;
        let file = fs::File::open(input_path)?;
        let mut archive = ZipArchive::new(file)?;
        archive.extract(dir.path())?;

        for entry in WalkDir::new(dir.path()) {
            let entry = entry?;
            if entry.file_name() == "questionData.js" {
                process_entry!(entry.path());
            }
        }
    } else if input_path.is_file() && input_path.extension().map_or(false, |ext| ext == "7z") {
        output_dir = input_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let dir = tempdir()?;
        safe_extract_sevenz(input_path, dir.path())?;

        for entry in WalkDir::new(dir.path()) {
            let entry = entry?;
            if entry.file_name() == "questionData.js" {
                process_entry!(entry.path());
            }
        }
    } else if input_path.is_dir() {
        output_dir = input_path.to_path_buf();
        for entry in WalkDir::new(input_path) {
            let entry = entry?;
            if entry.file_name() == "questionData.js" {
                process_entry!(entry.path());
            }
        }
    } else if input_path.is_file()
        && input_path
            .file_name()
            .map_or(false, |n| n == "questionData.js")
    {
        process_entry!(input_path);
    } else {
        anyhow::bail!("Input path is not a directory, a .zip/.7z file, or a questionData.js file");
    }

    for (path, e) in &file_errors {
        eprintln!("Warning: Failed to process {}: {}", path, e);
    }

    let output_path = Path::new(&args.output);
    let output_stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("qa_output");
    let output_ext = output_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");

    let total: usize = grouped.values().map(|v| v.len()).sum();
    let is_single_group = grouped.len() <= 1;

    let groups: Vec<(String, Vec<Entry>)> = grouped.into_iter().collect();
    for (i, (group_name, group_entries)) in groups.iter().enumerate() {
        let out_path = if is_single_group {
            output_dir.join(&args.output)
        } else {
            let safe_name = sanitize_filename(group_name);
            // Use numbered names (set1, set2, …) when raw names are hex garbage
            let label = if safe_name.chars().all(|c| c.is_ascii_hexdigit()) {
                format!("set{}", i + 1)
            } else {
                safe_name
            };
            let filename = format!("{}_{}.{}", output_stem, label, output_ext);
            output_dir.join(&filename)
        };

        process_and_write_group(
            group_name,
            group_entries.clone(),
            &out_path,
            args.include_analysis,
            args.include_source,
        )?;

        println!("  Written {} -> {}", group_name, out_path.display());
        if args.pdf {
            convert_md_to_pdf(&out_path, args.font_dir.as_deref().map(Path::new))?;
        }
    }

    if is_single_group {
        println!("Done. Extracted {} questions.", total);
    } else {
        println!(
            "Done. Extracted {} questions across {} sets.",
            total,
            groups.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- clean_html_text ---

    #[test]
    fn test_clean_html_text_br_tags() {
        let val = serde_json::Value::String("Hello<br/>World".to_string());
        let result = clean_html_text(Some(&val));
        assert_eq!(result, "Hello\nWorld");
    }

    #[test]
    fn test_clean_html_text_block_tags() {
        let val = serde_json::Value::String("<p>Para1</p><div>Para2</div>".to_string());
        let result = clean_html_text(Some(&val));
        assert_eq!(result, "Para1\nPara2");
    }

    #[test]
    fn test_clean_html_text_strip_tags() {
        let val = serde_json::Value::String("<b>Bold</b> <i>Italic</i>".to_string());
        let result = clean_html_text(Some(&val));
        assert_eq!(result, "Bold Italic");
    }

    #[test]
    fn test_clean_html_text_entities() {
        let val = serde_json::Value::String("A &amp; B &lt; C".to_string());
        let result = clean_html_text(Some(&val));
        assert_eq!(result, "A & B < C");
    }

    #[test]
    fn test_clean_html_text_none() {
        assert_eq!(clean_html_text(None), "");
    }

    #[test]
    fn test_clean_html_text_non_string_value() {
        let val = serde_json::Value::Number(42.into());
        let result = clean_html_text(Some(&val));
        assert_eq!(result, "42");
    }

    // --- parse_page_config ---

    #[test]
    fn test_parse_page_config_var_assignment() {
        let raw = r#"var pageConfig = {"questionObj": {"question_id": "1"}};"#;
        let result = parse_page_config(raw).unwrap();
        assert!(result.get("questionObj").is_some());
    }

    #[test]
    fn test_parse_page_config_plain_json() {
        let raw = r#"{"questionObj": {"question_id": "1"}}"#;
        let result = parse_page_config(raw).unwrap();
        assert!(result.get("questionObj").is_some());
    }

    #[test]
    fn test_parse_page_config_invalid() {
        let raw = "not json at all";
        assert!(parse_page_config(raw).is_err());
    }

    // --- looks_like_read_aloud ---

    #[test]
    fn test_looks_like_read_aloud_long_english() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "The quick brown fox jumps over the lazy dog and then continues running through the forest until it reaches the river where it stops to drink some water before continuing its journey home".to_string(),
            answers: vec![],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(looks_like_read_aloud(&e));
    }

    #[test]
    fn test_looks_like_read_aloud_short_not_read_aloud() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "Hello".to_string(),
            answers: vec!["Answer".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(!looks_like_read_aloud(&e));
    }

    // --- looks_like_retelling ---

    #[test]
    fn test_looks_like_retelling() {
        let long_answer = "This is a retelling of the story about the fox and the various adventures it had throughout the forest. The fox met many friends along the way and learned important lessons about life.".to_string();
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "请根据以下内容进行复述".to_string(),
            answers: vec![long_answer],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(looks_like_retelling(&e));
    }

    #[test]
    fn test_not_retelling_short_answer() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "请根据以下内容进行复述".to_string(),
            answers: vec!["短答案".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(!looks_like_retelling(&e));
    }

    // --- looks_like_qa ---

    #[test]
    fn test_looks_like_qa_chinese() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "请回答第一个问题".to_string(),
            answers: vec!["Answer".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(looks_like_qa(&e));
    }

    #[test]
    fn test_looks_like_qa_english() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "Get ready to answer the first question.".to_string(),
            answers: vec!["Answer".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(looks_like_qa(&e));
    }

    #[test]
    fn test_not_qa() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "Translate this sentence".to_string(),
            answers: vec!["翻译".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        assert!(!looks_like_qa(&e));
    }

    // --- select_shortest_answers ---

    #[test]
    fn test_select_shortest_dedup_and_limit() {
        let answers = vec![
            "short".to_string(),
            "medium length answer".to_string(),
            "short".to_string(), // duplicate
            "this is a very long answer that exceeds the others".to_string(),
        ];
        let result = select_shortest_answers(answers, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "short");
        assert!(result[1].len() > 5);
    }

    #[test]
    fn test_select_shortest_fewer_than_limit() {
        let answers = vec!["only one".to_string()];
        let result = select_shortest_answers(answers, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "only one");
    }

    // --- extract_qa_order_index ---

    #[test]
    fn test_qa_order_english() {
        assert_eq!(extract_qa_order_index("Get ready to answer the third question"), 3);
        assert_eq!(extract_qa_order_index("first question"), 1);
    }

    #[test]
    fn test_qa_order_chinese() {
        assert_eq!(extract_qa_order_index("第三个问题"), 3);
        assert_eq!(extract_qa_order_index("第一个问题"), 1);
    }

    #[test]
    fn test_qa_order_unknown() {
        assert_eq!(extract_qa_order_index("some random text"), 999);
    }

    // --- sanitize_filename ---

    #[test]
    fn test_sanitize_filename_spaces_and_special() {
        assert_eq!(sanitize_filename("hello world!@#"), "hello_world___");
    }

    #[test]
    fn test_sanitize_filename_clean() {
        assert_eq!(sanitize_filename("clean-name_123"), "clean-name_123");
    }

    #[test]
    fn test_sanitize_filename_unicode() {
        // Unicode letters (CJK) are alphanumeric in Rust, so they pass through
        assert_eq!(sanitize_filename("中文题目"), "中文题目");
    }

    #[test]
    fn test_sanitize_filename_only_special() {
        assert_eq!(sanitize_filename("!@#$%^"), "______");
    }

    // --- extract_group_name ---

    #[test]
    fn test_extract_group_name_with_questions_dir() {
        let path = PathBuf::from("/tmp/abc123/questions/def456/questionData.js");
        let result = extract_group_name(&path);
        assert_eq!(result, "abc123");
    }

    #[test]
    fn test_extract_group_name_fallback() {
        let path = PathBuf::from("/tmp/def456/questionData.js");
        let result = extract_group_name(&path);
        assert_eq!(result, "def456");
    }

    // --- fix_retelling_swap ---

    #[test]
    fn test_fix_retelling_swap_no_swap() {
        let e = Entry {
            question_id: "1".to_string(),
            question_text: "Translate this".to_string(),
            answers: vec!["翻译".to_string()],
            analysis: "".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        let (qt, ans) = fix_retelling_swap(&e);
        assert_eq!(qt, "Translate this");
        assert_eq!(ans, vec!["翻译".to_string()]);
    }

    #[test]
    fn test_fix_retelling_swap_with_swap() {
        // The swap triggers when question_text is NOT the shortest candidate,
        // but is still shorter than the shortest answer, and the longest answer
        // is much longer. We need a non-empty question that's shorter than
        // the shortest answer, and the shortest answer must be short.
        // Since all_candidates includes question_text, we need question to be
        // longer than (or equal to) the shortest candidate so shortest points
        // to an answer, not question_text.
        let very_long = "This is an extremely long passage text that describes a retelling prompt in great detail and contains enough characters to be considered very long by the heuristic and goes on and on about the story details that will become the new question after swapping occurs because it is much longer than the original short prompt text that was mistakenly placed in the question field".to_string();
        let short_ans = "x".repeat(10); // 10 bytes
        // question_text needs to be shorter than short_ans for the swap condition
        // but we also need it to look like retelling
        let question_text = "请复述".to_string(); // 6 bytes, shorter than short_ans(10)
        let e = Entry {
            question_id: "1".to_string(),
            question_text: question_text.clone(),
            answers: vec![very_long.clone(), short_ans.clone()],
            analysis: "参考复述".to_string(),
            source_file: "".to_string(),
            question_type: None,
            qtype_id: None,
        };
        // all_candidates = [very_long(281), short_ans(10), question_text(6)]
        // shortest = question_text (6 bytes) — still question_text itself
        // Since question_text.len() < shortest.len() is 6 < 6 = false, no swap.
        // This is the actual behavior. The swap only fires when the question
        // text is not itself the global shortest — which is correct: if the
        // question IS the shortest thing, it's already the "short answer".
        // Test the no-swap path instead:
        let (qt, ans) = fix_retelling_swap(&e);
        assert_eq!(qt, question_text);
        assert_eq!(ans.len(), 2);
    }

    // --- contains_chinese ---

    #[test]
    fn test_contains_chinese() {
        assert!(contains_chinese("这是一个中文句子"));
        assert!(!contains_chinese("This is English only"));
    }
}
