//! batstone - a coding agent that evolves itself
//!
//! Commands:
//!   /quit, /exit    Exit the agent
//!   /clear          Clear conversation history
//!   /model <name>   Switch model mid-session
//!   /help           Show available commands

use rig::agent::Agent;
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::{CompletionModel, Prompt, ToolDefinition};
use rig::providers::openrouter;
use rig::tool::{Tool, ToolError};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::to_value;
use std::io;
use std::io::{BufRead, IsTerminal, Read, Write};
use tokio::process::Command;

// ANSI colour helpers
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help() {
    println!("\n{BOLD}batstone{RESET} v{VERSION} — an evolving coding agent\n");
    println!("{BOLD}USAGE:{RESET}");
    println!("  batstone [OPTIONS]\n");
    println!("{BOLD}OPTIONS:{RESET}");
    println!("  --model <name>     Model to use (default: openai/gpt-4o)");
    println!("  --skills <dir>     Directory containing skill definitions");
    println!("  --version          Print version and exit");
    println!("  --help             Print this help and exit\n");
    println!("{BOLD}COMMANDS (in REPL):{RESET}");
    println!("  /help              Show this help");
    println!("  /clear             Clear conversation history");
    println!("  /model <name>      Switch model mid-session");
    println!("  /quit, /exit       Exit the agent\n");
}

const SYSTEM_PROMPT: &str = r#"You are a coding assistant working in the user's terminal.
You have access to the filesystem and shell. Be direct and concise.
When the user asks you to do something, do it — don't just explain how.
Use tools proactively: read files to understand context, run commands to verify your work.
After making changes, run tests or verify the result when appropriate."#;

fn print_banner() {
    println!("\n{BOLD}{CYAN}  batstone{RESET} {DIM}— our evolving coding agent{RESET}");
    println!("{DIM}  Type /quit to exit, /clear to reset{RESET}\n");
}


#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // Read environment arguments
    let args: Vec<String> = std::env::args().collect();

    // Handle --help flag (check before creating client to avoid env var requirement)
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // Handle --version flag
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("batstone {VERSION}");
        return Ok(());
    }

    // Creates OpenRouter client
    let client = openrouter::Client::from_env();

    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "openai/gpt-4o".into());

    let skill_dirs: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--skills")
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect();

    let skills = load_skills(&skill_dirs);
    let skills_prompt = format_skills_xml(&skills);

    let full_system_prompt = format!("{}{}", SYSTEM_PROMPT, skills_prompt);

    // Helper to build a fresh agent (used for initial build and /clear resets)
    let build_agent = |model: &str| {
        client
            .agent(model)
            .preamble(&full_system_prompt)
            .tools(vec![
                Box::new(ListFilesTool),
                Box::new(ReadFileTool),
                Box::new(WriteFileTool),
                Box::new(BashTool),
                Box::new(EditFileTool),
                Box::new(SearchTool),
            ])
            .default_max_turns(1000)
            .build()
    };

    let mut current_model = model.clone();
    let mut agent = build_agent(&current_model);

    // Piped mode: read all of stdin as a single prompt, run once, exit
    if !io::stdin().is_terminal() {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).ok();
        let input = input.trim();
        if input.is_empty() {
            eprintln!("No input on stdin.");
            std::process::exit(1);
        }

        eprintln!("{DIM}  batstone (piped mode) — model: {current_model}{RESET}");
        run_prompt(&mut agent, input).await;
        return Ok(());
    }

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unknown)".into());

    print_banner();
    println!("{DIM}  model: {current_model}{RESET}");
    if !skills.is_empty() {
        println!("{DIM}  skills: {} loaded{RESET}", skills.len());
    }
    println!("{DIM}  cwd:   {cwd}{RESET}\n");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("{BOLD}{GREEN}> {RESET}");
        io::stdout().flush().ok();

        let line = match lines.next() {
            Some(Ok(l)) => l,
            _ => break,
        };

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }

        // Handle slash commands
        if let Some(cmd) = parse_command(&input) {
            match cmd {
                ReplCommand::Quit => break,
                ReplCommand::Help => {
                    print_help();
                }
                ReplCommand::Clear => {
                    agent = build_agent(&current_model);
                    println!("{DIM}  conversation cleared{RESET}");
                }
                ReplCommand::Model(None) => {
                    println!("{DIM}  current model: {current_model}{RESET}");
                }
                ReplCommand::Model(Some(new_model)) => {
                    current_model = new_model.to_string();
                    agent = build_agent(&current_model);
                    println!("{DIM}  switched to model: {current_model}{RESET}");
                }
                ReplCommand::Unknown(name) => {
                    println!("{YELLOW}  unknown command: {name}{RESET}");
                    println!("{DIM}  type /help for available commands{RESET}");
                }
            }
            continue;
        }

        run_prompt(&mut agent, &input).await;
    }

    println!("\n{DIM}  bye 👋{RESET}\n");

    Ok(())
}

async fn run_prompt<T: CompletionModel>(agent: &mut Agent<T>, input: &str) {
    // Show a "thinking" indicator while waiting for the model.
    eprint!("{DIM}  thinking… (Ctrl+C to cancel){RESET}");
    let _ = io::stderr().flush();

    // Race the model call against a Ctrl+C signal so the user can cancel
    // a long-running turn without killing the whole process.
    tokio::select! {
        result = agent.prompt(input) => {
            eprint!("\r\x1b[2K");
            match result {
                Ok(response) => println!("{}", response),
                Err(e) => eprintln!("{RED}  error: {e}{RESET}"),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprint!("\r\x1b[2K");
            eprintln!("{YELLOW}  cancelled{RESET}");
        }
    }
}

#[derive(Debug)]
struct Skill {
    name: String,
    description: String,
    location: String,
    tools: Vec<String>,
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    tools: Option<Vec<String>>,
}

/// Parses a SKILL.md file to extract its YAML frontmatter
fn parse_skill(path: &std::path::Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;

    // Split the file by "---" to isolate the frontmatter.
    let parts: Vec<&str> = content.splitn(3, "---").collect();

    if parts.len() == 3 {
        let yaml_str = parts[1];
        // Parse the YAML block into our struct
        if let Ok(frontmatter) = serde_yaml::from_str::<SkillFrontmatter>(yaml_str) {
            return Some(Skill {
                name: frontmatter.name,
                description: frontmatter.description,
                location: path.to_string_lossy().to_string(),
                // Convert Option<Vec<String>> to Vec<String>, defaulting to empty
                tools: frontmatter.tools.unwrap_or_default(),
            });
        }
    }
    None
}

/// Scans the provided directories for `skill_name/SKILL.md` structures
fn load_skills(skill_dirs: &[String]) -> Vec<Skill> {
    let mut skills = Vec::new();

    for dir in skill_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let skill_file = path.join("SKILL.md");
                    if skill_file.exists() {
                        if let Some(skill) = parse_skill(&skill_file) {
                            skills.push(skill);
                        }
                    }
                }
            }
        }
    }
    skills
}

/// Formats the loaded skills into the XML string for the system prompt
fn format_skills_xml(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut xml = String::from("\n\n<available_skills>\n");
    for skill in skills {
        xml.push_str("  <skill>\n");
        xml.push_str(&format!("    <name>{}</name>\n", skill.name));
        xml.push_str(&format!("    <description>{}</description>\n", skill.description));
        xml.push_str(&format!("    <location>{}</location>\n", skill.location));

        if !skill.tools.is_empty() {
            xml.push_str(&format!("    <tools>{}</tools>\n", skill.tools.join(", ")));
        }

        xml.push_str("  </skill>\n");
    }
    xml.push_str("</available_skills>");

    xml
}

struct ReadFileTool;

#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadFileToolArgs {
    /// The path to the file you want to read
    pub path: String,
}
impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Error = ToolError;
    type Args = ReadFileToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Reads the contents of a file.".to_string(),
            parameters: to_value(schema_for!(ReadFileToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::fs::read_to_string(&args.path)
            .await
            .map_err(|e| ToolError::ToolCallError(
                format!("Failed to read file at {}: {}", args.path, e).into()
            ))
    }
}

struct WriteFileTool;

#[derive(Serialize, Deserialize, JsonSchema)]
struct WriteFileToolArgs {
    /// The path where the file should be written
    pub path: String,
    /// The complete content to write into the file
    pub content: String,
}
impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";
    type Error = ToolError;
    type Args = WriteFileToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Writes content to a file, creating it if it doesn't exist or overwriting the entire file contents it if it does.".to_string(),
            parameters: to_value(schema_for!(WriteFileToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tokio::fs::write(&args.path, &args.content)
            .await
            .map_err(|e| ToolError::ToolCallError(
                format!("Error writing to file '{}': Ensure the file path is correct and accessible. Error: {}", args.path, e).into()
            ))?;

        Ok(format!("Successfully wrote {} bytes to {}", args.content.len(), args.path))
    }
}

struct BashTool;

#[derive(Serialize, Deserialize, JsonSchema)]
/// Arguments to parse to the `BashTool`
///
/// * `command`: The command to run
/// * `cwd`: Working directory to run the command from (defaults to current directiry)
struct BashToolArgs {
    /// Command to run
    pub command: String,
    /// Current working directory in which to invoke the command
    pub cwd: Option<String>,
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = ToolError;
    type Args = BashToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Runs a bash command and returns its stdout and stderr".to_string(),
            parameters: to_value(schema_for!(BashToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(&args.command);
        if let Some(cwd) = &args.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd
            .output()
            .await
            .map_err(|e| ToolError::ToolCallError(e.to_string().into()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("[stderr]\n");
            result.push_str(&stderr);
        }
        if exit_code != 0 {
            result.push_str(&format!("\n[exit code: {exit_code}]"));
        }

        Ok(result)
    }
}

pub struct ListFilesTool;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListFilesToolArgs {
    /// The directory path to start searching in (e.g., "." for current directory)
    pub path: String,
    /// Optional wildcard pattern to match file names (e.g., "*.rs" or "*.json")
    pub pattern: Option<String>,
    /// Optional maximum depth for directory traversal to prevent overwhelming output (e.g., 1 or 2)
    pub max_depth: Option<u32>,
}

impl Tool for ListFilesTool {
    const NAME: &'static str = "list_files";
    type Error = ToolError;
    type Args = ListFilesToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Lists files and directories using the `find` command. Use max_depth to prevent excessive output in large repositories.".to_string(),
            parameters: to_value(schema_for!(ListFilesToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut cmd = Command::new("find");

        // Start path
        cmd.arg(&args.path);

        // Apply max depth if provided
        if let Some(depth) = args.max_depth {
            cmd.arg("-maxdepth").arg(depth.to_string());
        }

        // Apply name pattern if provided
        if let Some(pattern) = &args.pattern {
            cmd.arg("-name").arg(pattern);
        }

        // Execute the command asynchronously
        let output = cmd.output().await.map_err(|e| {
            ToolError::ToolCallError(format!("Failed to spawn find command: {}", e).into())
        })?;

        // Check if the command succeeded
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

            if stdout.trim().is_empty() {
                Ok("No files found matching the criteria.".to_string())
            } else {
                Ok(stdout)
            }
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(ToolError::ToolCallError(format!(
                "find command returned an error: {}",
                stderr.trim()
            ).into()))
        }
    }
}

pub struct EditFileTool;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EditFileToolArgs {
    /// The path to the file you want to edit
    pub path: String,
    /// The exact text to find in the file. This must match the file's contents perfectly, including all whitespace and indentation.
    pub old_text: String,
    /// The new text that will replace the old text.
    pub new_text: String,
}

impl Tool for EditFileTool {
    const NAME: &'static str = "edit_file";
    type Error = ToolError;
    type Args = EditFileToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Make a surgical edit to a file by specifying exact text to find and replace. The old_text must match exactly (including whitespace and indentation). For creating new files, use write_file instead.".to_string(),
            parameters: to_value(schema_for!(EditFileToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Read the current contents of the file
        let content = tokio::fs::read_to_string(&args.path)
            .await
            .map_err(|e| ToolError::ToolCallError(
                format!("Failed to read file {}: {}", args.path, e).into()
            ))?;

        // Validate that the search string actually exists in the file
        if !content.contains(&args.old_text) {
            return Err(ToolError::ToolCallError(
                format!(
                    "The exact search string was not found in {}. Ensure that whitespace, indentation, and line endings match the file exactly.",
                    args.path
                ).into()
            ));
        }

        // Perform the replacement
        let new_content = content.replace(&args.old_text, &args.new_text);

        // Write the modified content back to the file
        tokio::fs::write(&args.path, new_content)
            .await
            .map_err(|e| ToolError::ToolCallError(
                format!("Failed to write to file {}: {}", args.path, e).into()
            ))?;

        Ok(format!("Successfully edited {}. Replaced a block of {} characters.", args.path, args.old_text.len()))
    }
}

pub struct SearchTool;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct SearchToolArgs {
    /// The string or regex pattern to search for.
    pub pattern: String,
    /// The directory to search in (e.g., "." for the current directory).
    pub path: String,
    /// Number of context lines to include before and after the match. Default is usually 2.
    pub context_lines: Option<u32>,
    /// Optional file glob to filter by (e.g., "*.rs" or "*.md").
    pub file_pattern: Option<String>,
}

impl Tool for SearchTool {
    const NAME: &'static str = "search";
    type Error = ToolError;
    type Args = SearchToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Searches for a text pattern in files using grep. Provides line numbers and context lines. Crucial for finding where functions or variables are defined and used.".to_string(),
            parameters: to_value(schema_for!(SearchToolArgs)).unwrap(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut cmd = Command::new("grep");

        // -r: recursive
        // -n: print line numbers (crucial for the LLM to know where to edit)
        // -I: ignore binary files (protects the context window)
        // -E: extended regex
        cmd.arg("-rnIE");

        // Add context lines
        let context = args.context_lines.unwrap_or(2);
        cmd.arg(format!("-C{}", context));

        // Filter by file pattern if provided
        if let Some(file_pattern) = &args.file_pattern {
            cmd.arg(format!("--include={}", file_pattern));
        }

        // The pattern and the target path
        cmd.arg(&args.pattern);
        cmd.arg(&args.path);

        let output = cmd.output().await.map_err(|e| {
            ToolError::ToolCallError(format!("Failed to spawn grep command: {}", e).into())
        })?;

        // grep exit codes:
        // 0 = One or more matches found
        // 1 = No matches found
        // >1 = Error
        match output.status.code() {
            Some(0) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                Ok(stdout)
            }
            Some(1) => Ok("No matches found.".to_string()),
            _ => {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                Err(ToolError::ToolCallError(format!(
                    "Search command failed: {}",
                    stderr.trim()
                ).into()))
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Represents a parsed REPL slash command.
#[derive(Debug, PartialEq)]
enum ReplCommand<'a> {
    Quit,
    Help,
    Clear,
    /// /model with an optional argument. None means "show current model".
    Model(Option<&'a str>),
    Unknown(&'a str),
}

/// Parses a slash-command line (e.g. "/model gpt-4o") into a `ReplCommand`.
/// Returns `None` if the input is not a slash command.
fn parse_command(input: &str) -> Option<ReplCommand<'_>> {
    if !input.starts_with('/') {
        return None;
    }
    let (cmd, rest) = match input.find(' ') {
        Some(pos) => (&input[..pos], input[pos + 1..].trim()),
        None => (input, ""),
    };
    let parsed = match cmd {
        "/quit" | "/exit" => ReplCommand::Quit,
        "/help" => ReplCommand::Help,
        "/clear" => ReplCommand::Clear,
        "/model" => {
            if rest.is_empty() {
                ReplCommand::Model(None)
            } else {
                ReplCommand::Model(Some(rest))
            }
        }
        other => ReplCommand::Unknown(other),
    };
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- truncate tests ---

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_unicode() {
        assert_eq!(truncate("héllo wörld", 5), "héllo");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 5), "");
    }

    // --- parse_command tests ---

    #[test]
    fn test_parse_command_not_a_command() {
        assert_eq!(parse_command("hello world"), None);
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("  /quit"), None); // leading space — not a command
    }

    #[test]
    fn test_parse_command_quit() {
        assert_eq!(parse_command("/quit"), Some(ReplCommand::Quit));
        assert_eq!(parse_command("/exit"), Some(ReplCommand::Quit));
    }

    #[test]
    fn test_parse_command_help() {
        assert_eq!(parse_command("/help"), Some(ReplCommand::Help));
    }

    #[test]
    fn test_parse_command_clear() {
        assert_eq!(parse_command("/clear"), Some(ReplCommand::Clear));
    }

    #[test]
    fn test_parse_command_model_no_arg() {
        assert_eq!(parse_command("/model"), Some(ReplCommand::Model(None)));
    }

    #[test]
    fn test_parse_command_model_with_arg() {
        assert_eq!(
            parse_command("/model openai/gpt-4o"),
            Some(ReplCommand::Model(Some("openai/gpt-4o")))
        );
    }

    #[test]
    fn test_parse_command_model_trims_spaces() {
        assert_eq!(
            parse_command("/model   anthropic/claude-3-5-sonnet"),
            Some(ReplCommand::Model(Some("anthropic/claude-3-5-sonnet")))
        );
    }

    #[test]
    fn test_parse_command_unknown() {
        assert_eq!(
            parse_command("/foobar"),
            Some(ReplCommand::Unknown("/foobar"))
        );
    }

    // --- format_skills_xml tests ---

    #[test]
    fn test_format_skills_xml_empty() {
        assert_eq!(format_skills_xml(&[]), "");
    }

    #[test]
    fn test_format_skills_xml_single_skill() {
        let skills = vec![Skill {
            name: "test-skill".to_string(),
            description: "A test skill".to_string(),
            location: "./skills/test/SKILL.md".to_string(),
            tools: vec!["bash".to_string(), "read_file".to_string()],
        }];
        let xml = format_skills_xml(&skills);
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("</available_skills>"));
        assert!(xml.contains("<name>test-skill</name>"));
        assert!(xml.contains("<description>A test skill</description>"));
        assert!(xml.contains("<tools>bash, read_file</tools>"));
    }

    #[test]
    fn test_format_skills_xml_no_tools() {
        let skills = vec![Skill {
            name: "minimal".to_string(),
            description: "No tools".to_string(),
            location: "./SKILL.md".to_string(),
            tools: vec![],
        }];
        let xml = format_skills_xml(&skills);
        // Should not emit an empty <tools> tag
        assert!(!xml.contains("<tools>"));
    }
}