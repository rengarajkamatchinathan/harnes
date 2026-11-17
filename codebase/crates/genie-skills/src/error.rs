use std::io;

/// Errors from skills, packages and the directory. Messages are model/user readable.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("no skill named `{0}`")]
    NotFound(String),
    #[error("skill `{0}` is disabled")]
    Disabled(String),
    #[error("skill `{0}` is blocked by your organisation's policy")]
    Blocked(String),
    #[error("skill `{0}` can only be started by the user")]
    UserOnly(String),
    #[error("invalid skill: {}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error("invalid skill package: {0}")]
    Package(String),
    #[error("path `{0}` is outside the skill folder")]
    PathEscape(String),
    #[error("{what} is too large ({size} bytes, limit {limit})")]
    TooLarge { what: String, size: u64, limit: u64 },
    #[error("a skill named `{0}` is already installed")]
    AlreadyExists(String),
    #[error("{0}")]
    Policy(String),
    #[error("skills directory: {0}")]
    Directory(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl SkillError {
    /// Stable code for logs and the wire (`error.data.code`).
    pub fn code(&self) -> &'static str {
        match self {
            SkillError::NotFound(_) => "skill_not_found",
            SkillError::Disabled(_) => "skill_disabled",
            SkillError::Blocked(_) | SkillError::Policy(_) => "skill_policy",
            SkillError::UserOnly(_) => "skill_user_only",
            SkillError::Invalid(_) => "skill_invalid",
            SkillError::Package(_) | SkillError::TooLarge { .. } => "skill_package_invalid",
            SkillError::PathEscape(_) => "skill_path_escape",
            SkillError::AlreadyExists(_) => "skill_exists",
            SkillError::Directory(_) => "skill_directory",
            SkillError::Io(_) => "io",
        }
    }
}
