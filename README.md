# txget

`txget` 是一个用 Rust 编写的命令行工具，用于从天学网导出的 `questionData.js` 中提取题目与参考答案并生成 Markdown 文档。

特性:

- 支持输入为目录、单个 `questionData.js` 文件或包含该文件的 `.zip`。
- 自动清理 HTML、提取题目、候选答案与解析（若存在），按题型分节输出。
- 可选包含解析（analysis）与来源文件路径。

## 构建

```bash
# 在项目根目录构建 release 版本
cargo build --release

# 可选：安装到本地 cargo bin
cargo install --path .
```

## 快速开始

[如何获取从天学网导出的 `questionData.js`？](./how-to-extract-raw-file.md)

```bash
# 在当前目录扫描并生成默认输出（默认文件名：qa_output.md）
./target/release/txget

# 指定输入为 zip，并指定输出文件名
./target/release/txget -f /path/to/archive.zip -o output.md

# 指定输入为目录
./target/release/txget -f /path/to/extracted/dir -o myout.md

# 指定单个 questionData.js 文件
./target/release/txget -f /path/to/questionData.js -o out.md
```

命令行参数

- `-f, --file <FILE>`: 输入路径（目录、.zip 或 questionData.js），默认 `.`（当前目录）。
- `-o, --output <OUTPUT>`: 输出 Markdown 文件名，默认 `qa_output.md`。
- `--include-analysis`: 是否在输出中包含解析/线索（默认 `false`）。
- `--include-source`: 是否在输出中包含来源文件路径（默认 `false`）。

输出说明
输出为 Markdown 文件，包含分节统计（朗读、翻译、问答、Retelling 等）和每题的题目、参考答案、可选解析与来源路径，便于人工校对与后续编辑。

## 许可

本项目使用 MIT 许可证，详见 LICENSE 文件。
