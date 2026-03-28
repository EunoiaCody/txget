use anyhow::{Context, Result};
use clap::Parser;
use html_escape::decode_html_entities;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use walkdir::WalkDir;
use zip::ZipArchive;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Extract questions and answers from questionData.js files"
)]
struct Args {
    /// Input root directory or a .zip file
    #[arg(short, long, default_value = ".")]
    file: String,

    /// Output Markdown file path
    #[arg(short, long, default_value = "qa_output.md")]
    output: String,

    /// Include analysis field when available
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    include_analysis: bool,

    /// Include source file path for each question
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    include_source: bool,
}

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

    let br_re = Regex::new(r"(?i)<\s*br\s*/?\s*>").unwrap();
    let block_re = Regex::new(r"(?i)</\s*(p|div|li|h[1-6])\s*>").unwrap();
    let tag_re = Regex::new(r"<[^>]+>").unwrap();

    let s = br_re.replace_all(&s, "\n");
    let s = block_re.replace_all(&s, "\n");
    let s = tag_re.replace_all(&s, "");
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

    let zh_re = Regex::new(r"第([一二三四五六七八九十])个问题").unwrap();
    if let Some(caps) = zh_re.captures(question_text) {
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
    let qa_re = Regex::new(r"(?i)get ready to answer the (first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth) question").unwrap();
    qa_re.is_match(&q)
}

fn contains_chinese(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn render_entry(e: &Entry, include_analysis: bool, include_source: bool) -> String {
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
    out.push_str(if e.question_text.is_empty() {
        "_（空）_"
    } else {
        &e.question_text
    });
    out.push_str("\n\n#### 参考答案\n");
    if e.answers.is_empty() {
        out.push_str("_未提取到可见答案_\n");
    } else {
        for (i, a) in e.answers.iter().enumerate() {
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

fn main() -> Result<()> {
    let args = Args::parse();
    let input_path = Path::new(&args.file);
    let mut entries = Vec::new();
    let mut output_dir = PathBuf::from(".");

    if input_path.is_file() && input_path.extension().map_or(false, |ext| ext == "zip") {
        output_dir = input_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let dir = tempdir()?;
        let file = fs::File::open(input_path)?;
        let mut archive = ZipArchive::new(file)?;
        archive.extract(dir.path())?;

        for entry in WalkDir::new(dir.path()) {
            let entry = entry?;
            if entry.file_name() == "questionData.js" {
                entries.append(&mut process_file(entry.path())?);
            }
        }
    } else if input_path.is_dir() {
        output_dir = input_path.to_path_buf();
        for entry in WalkDir::new(input_path) {
            let entry = entry?;
            if entry.file_name() == "questionData.js" {
                entries.append(&mut process_file(entry.path())?);
            }
        }
    } else if input_path.is_file()
        && input_path
            .file_name()
            .map_or(false, |n| n == "questionData.js")
    {
        entries.append(&mut process_file(input_path)?);
    } else {
        anyhow::bail!("Input path is not a directory, a .zip file, or a questionData.js file");
    }

    let mut read_aloud = Vec::new();
    let mut translation = Vec::new();
    let mut qa = Vec::new();
    let mut retelling = Vec::new();
    let mut others = Vec::new();

    for e in entries.clone() {
        if looks_like_read_aloud(&e) {
            read_aloud.push(e);
        } else if looks_like_retelling(&e) {
            retelling.push(e);
        } else if looks_like_qa(&e) {
            qa.push(e);
        } else if contains_chinese(&e.question_text)
            && e.answers
                .iter()
                .any(|a| a.chars().any(|c| c.is_ascii_alphabetic()))
        {
            translation.push(e);
        } else {
            others.push(e);
        }
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

    let mut markdown = format!("# 题目与答案提取结果\n\n");
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
        ("## 第一部分：跟随文章朗读", read_aloud),
        ("## 第二部分：翻译题（中文题目 -> 英文答案）", translation),
        ("## 第三部分：问答题（按第几个问题顺序）", qa),
        ("## 第四部分：Retelling", retelling),
    ];

    for (title, sec_entries) in sections {
        markdown.push_str(title);
        markdown.push_str("\n\n");
        for e in sec_entries {
            markdown.push_str(&render_entry(
                &e,
                args.include_analysis,
                args.include_source,
            ));
        }
    }

    if !others.is_empty() {
        markdown.push_str("## 其他未归类\n\n");
        for e in others {
            markdown.push_str(&render_entry(
                &e,
                args.include_analysis,
                args.include_source,
            ));
        }
    }

    let out_path = output_dir.join(&args.output);
    fs::write(&out_path, markdown)?;

    println!("Done. Extracted {} questions.", entries.len());
    println!("Output: {}", out_path.display());

    Ok(())
}
