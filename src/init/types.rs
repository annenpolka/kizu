use std::fmt;
use std::path::PathBuf;

/// Supported AI coding agents for hook installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Cursor,
    Codex,
    QwenCode,
    Cline,
    Gemini,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => write!(f, "Claude Code"),
            Self::Cursor => write!(f, "Cursor"),
            Self::Codex => write!(f, "Codex CLI"),
            Self::QwenCode => write!(f, "Qwen Code"),
            Self::Cline => write!(f, "Cline"),
            Self::Gemini => write!(f, "Gemini CLI"),
        }
    }
}

impl AgentKind {
    pub fn all() -> &'static [AgentKind] {
        &[
            Self::ClaudeCode,
            Self::Cursor,
            Self::Codex,
            Self::QwenCode,
            Self::Cline,
            Self::Gemini,
        ]
    }

    /// CLI name for `--agent` flag parsing.
    #[allow(dead_code)]
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::QwenCode => "qwen",
            Self::Cline => "cline",
            Self::Gemini => "gemini",
        }
    }

    pub fn from_cli_name(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "cursor" => Some(Self::Cursor),
            "codex" => Some(Self::Codex),
            "qwen" | "qwen-code" => Some(Self::QwenCode),
            "cline" => Some(Self::Cline),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub(super) fn binary_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::QwenCode => "qwen",
            Self::Cline => "cline", // not a real binary, detected by config dir
            Self::Gemini => "gemini",
        }
    }

    /// Project-local config directory (relative to worktree root).
    /// `None` if this agent only has a user-level config.
    pub(super) fn project_config_dir(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some(".claude"),
            Self::Cursor => Some(".cursor"),
            Self::QwenCode => Some(".qwen"),
            Self::Cline => Some(".clinerules"),
            Self::Codex | Self::Gemini => None,
        }
    }

    /// User-level config directory (absolute). `None` if this agent
    /// only has project-level config.
    pub(super) fn user_config_dir(self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        match self {
            Self::Codex => Some(home.join(".codex")),
            Self::Gemini => Some(home.join(".gemini")),
            Self::ClaudeCode => Some(home.join(".claude")),
            Self::Cursor => None, // cursor user config is different path
            Self::QwenCode => None,
            Self::Cline => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    /// PostToolUse + Stop hooks both available.
    Full,
    /// Only Stop hook (Codex: PreTool/PostTool Bash-only).
    StopOnly,
    /// PostToolUse only, no Stop gate (Cline).
    PostToolOnlyBestEffort,
    /// No hook mechanism; stream/scar-only (Gemini).
    WriteSideOnly,
}

impl fmt::Display for SupportLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "Full"),
            Self::StopOnly => write!(f, "Stop only"),
            Self::PostToolOnlyBestEffort => write!(f, "PostTool best-effort: no Stop gate"),
            Self::WriteSideOnly => write!(f, "Write-side only"),
        }
    }
}

pub fn support_level(kind: AgentKind) -> SupportLevel {
    match kind {
        AgentKind::ClaudeCode | AgentKind::Cursor | AgentKind::QwenCode => SupportLevel::Full,
        AgentKind::Codex => SupportLevel::StopOnly,
        AgentKind::Cline => SupportLevel::PostToolOnlyBestEffort,
        AgentKind::Gemini => SupportLevel::WriteSideOnly,
    }
}

#[derive(Debug, Clone)]
pub struct DetectedAgent {
    pub kind: AgentKind,
    pub binary_found: bool,
    pub config_dir_found: bool,
    pub recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `.claude/settings.local.json` etc. — gitignored, personal.
    ProjectLocal,
    /// `.claude/settings.json` etc. — committed, team-shared.
    ProjectShared,
    /// `~/.claude/settings.json` etc. — global user config.
    User,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectLocal => write!(f, "project-local"),
            Self::ProjectShared => write!(f, "project-shared"),
            Self::User => write!(f, "user"),
        }
    }
}

#[derive(Debug)]
pub struct InstallReport {
    pub agent: AgentKind,
    pub files_modified: Vec<PathBuf>,
    pub entries_added: usize,
    pub entries_skipped: usize,
    pub warnings: Vec<String>,
}
