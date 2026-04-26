use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "txget",
    author,
    version,
    about = "Extract questions and answers from questionData.js files"
)]
pub struct Args {
    /// Input root directory or a .zip file
    #[arg(short, long, default_value = ".")]
    pub file: String,

    /// Output Markdown file path
    #[arg(short, long, default_value = "qa_output.md")]
    pub output: String,

    /// Include analysis field when available
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub include_analysis: bool,

    /// Include source file path for each question
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub include_source: bool,
}
