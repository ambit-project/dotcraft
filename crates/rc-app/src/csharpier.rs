//! Reformat Code for C# with [CSharpier](https://csharpier.com) instead of the language server.
//!
//! CSharpier is found, in order: the path set in Settings › Tools, a local tool in the
//! `dotnet-tools.json` manifest of the file's repository (`dotnet csharpier`), then a global
//! `csharpier` on PATH or in `~/.dotnet/tools`. The document is piped to
//! `csharpier format --stdin-path <file>` (CSharpier 1.0 or later) on the background runtime,
//! so the file's `.csharpierrc`, `.editorconfig` and `.csharpierignore` apply.

use rc_core::settings::CSharpFormatter;
use rc_core::{Inbox, Runtime, Waker};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// How to start CSharpier: a program and the arguments before `format`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub prefix: Vec<String>,
}

/// A finished format of document `doc` at buffer `version`.
pub struct Formatted {
    pub doc: u64,
    pub version: u64,
    pub original: String,
    pub result: Result<String, String>,
    /// Save the document once the result is applied (Reformat on save).
    pub save: bool,
}

pub struct Csharpier {
    inbox: Inbox<Formatted>,
    /// Documents with a format in flight.
    pending: Vec<u64>,
}

impl Csharpier {
    pub fn new(waker: Waker) -> Self {
        Self { inbox: Inbox::new(waker), pending: Vec::new() }
    }

    pub fn is_pending(&self, doc: u64) -> bool {
        self.pending.contains(&doc)
    }

    /// Starts formatting `text` (document `doc` at `version`, saved at `path`).
    pub fn format(&mut self, command: Command, path: &Path, doc: u64, version: u64, text: String, save: bool) {
        if self.is_pending(doc) {
            return;
        }
        self.pending.push(doc);
        let out = self.inbox.sender();
        let path = path.to_path_buf();
        Runtime::spawn(async move {
            let result = run(&command, &path, &text).await;
            out.send(Formatted { doc, version, original: text, result, save });
        });
    }

    pub fn drain(&mut self) -> Vec<Formatted> {
        let done = self.inbox.drain();
        self.pending.retain(|d| !done.iter().any(|f| f.doc == *d));
        done
    }
}

async fn run(command: &Command, path: &Path, text: &str) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&command.program);
    cmd.args(&command.prefix).arg("format").arg("--stdin-path").arg(path);
    // Local tools and option files are found from the file's folder.
    if let Some(dir) = path.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.env("DOTNET_CLI_TELEMETRY_OPTOUT", "1").env("DOTNET_NOLOGO", "1");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", command.program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await.map_err(|e| e.to_string())?;
        drop(stdin);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| "CSharpier did not answer within 60 seconds".to_string())?
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() || (stdout.trim().is_empty() && !text.trim().is_empty()) {
        let message = if stderr.is_empty() { stdout.trim().to_string() } else { stderr };
        return Err(first_lines(&message, 6));
    }
    Ok(stdout)
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().filter(|l| !l.trim().is_empty()).take(n).collect::<Vec<_>>().join("\n")
}

/// The folder whose tool manifest lists CSharpier, from `file` upwards.
pub fn manifest_dir(file: &Path) -> Option<PathBuf> {
    file.ancestors().skip(1).find(|dir| {
        [dir.join(".config").join("dotnet-tools.json"), dir.join("dotnet-tools.json")]
            .iter()
            .any(|m| std::fs::read_to_string(m).is_ok_and(|t| t.to_ascii_lowercase().contains("\"csharpier\"")))
    }).map(Path::to_path_buf)
}

/// Whether the repository of `file` uses CSharpier: a local tool or a CSharpier option file.
pub fn project_uses_csharpier(file: &Path) -> bool {
    if manifest_dir(file).is_some() {
        return true;
    }
    const FILES: [&str; 5] = [".csharpierrc", ".csharpierrc.json", ".csharpierrc.yaml", ".csharpierrc.yml", ".csharpierignore"];
    file.ancestors().skip(1).any(|dir| FILES.iter().any(|f| dir.join(f).is_file()))
}

/// A global `csharpier` executable on PATH or in `~/.dotnet/tools`.
fn global_executable() -> Option<PathBuf> {
    let name = if cfg!(windows) { "csharpier.exe" } else { "csharpier" };
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join(name));
    let tools = dirs::home_dir().map(|h| h.join(".dotnet").join("tools").join(name));
    on_path.chain(tools).find(|p| p.is_file())
}

/// How to run CSharpier for `file`, or `None` when it is not installed.
pub fn resolve(file: &Path, configured: Option<&Path>, dotnet: &Path) -> Option<Command> {
    if let Some(p) = configured.filter(|p| p.is_file()) {
        return Some(Command { program: p.to_path_buf(), prefix: Vec::new() });
    }
    if manifest_dir(file).is_some() {
        return Some(Command { program: dotnet.to_path_buf(), prefix: vec!["csharpier".into()] });
    }
    global_executable().map(|program| Command { program, prefix: Vec::new() })
}

/// Whether Reformat Code of C# `file` uses CSharpier under the `choice` setting.
pub fn applies(choice: CSharpFormatter, file: &Path) -> bool {
    match choice {
        CSharpFormatter::Roslyn => false,
        CSharpFormatter::CSharpier => true,
        CSharpFormatter::Auto => project_uses_csharpier(file),
    }
}

/// The smallest edit turning `old` into `new`: char offsets `(start, end)` in `old` and the
/// replacement text. `None` when they are equal.
pub fn minimal_change(old: &str, new: &str) -> Option<(usize, usize, String)> {
    if old == new {
        return None;
    }
    let (a, b): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a.iter().rev().zip(b.iter().rev()).take(max_suffix).take_while(|(x, y)| x == y).count();
    Some((prefix, a.len() - suffix, b[prefix..b.len() - suffix].iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_change_replaces_only_the_difference() {
        assert_eq!(minimal_change("abc", "abc"), None);
        assert_eq!(minimal_change("class A{}", "class A { }"), Some((7, 8, " { ".into())));
        assert_eq!(minimal_change("aaa", "aa"), Some((2, 3, String::new())));
        assert_eq!(minimal_change("żółw", "żółw\n"), Some((4, 4, "\n".into())));
    }

    #[test]
    fn finds_a_local_tool_and_option_files() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src").join("App");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("Program.cs");
        assert!(!project_uses_csharpier(&file));
        assert!(!applies(CSharpFormatter::Auto, &file));
        std::fs::write(root.path().join(".csharpierrc.json"), "{}").unwrap();
        assert!(project_uses_csharpier(&file));
        assert_eq!(manifest_dir(&file), None);
        std::fs::create_dir_all(root.path().join(".config")).unwrap();
        std::fs::write(root.path().join(".config/dotnet-tools.json"), r#"{"version":1,"tools":{"csharpier":{"version":"1.2.0","commands":["csharpier"]}}}"#).unwrap();
        assert_eq!(manifest_dir(&file).as_deref(), Some(root.path()));
        let cmd = resolve(&file, None, Path::new("/usr/bin/dotnet")).unwrap();
        assert_eq!(cmd, Command { program: "/usr/bin/dotnet".into(), prefix: vec!["csharpier".into()] });
        assert!(applies(CSharpFormatter::Auto, &file));
        assert!(!applies(CSharpFormatter::Roslyn, &file));
    }
}
