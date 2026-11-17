//! Command files on fixture folders (compatible with the CLI's `load_user_commands`).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::fs;
use std::path::Path;

use genie_skills::command::{CommandScope, command_dirs, load_commands};

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn loads_user_and_trusted_project_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".genie");
    let ws = tmp.path().join("ws");
    write(
        &home.join("commands/review.md"),
        "---\ndescription: Review code\n---\nReview $ARGUMENTS",
    );
    write(&home.join("commands/git/pr.md"), "# Open a PR\nDo it");
    write(&home.join("commands/compact.md"), "shadowing a built-in");
    write(
        &home.join("commands/bad.md"),
        "---\ndescription: [unclosed\n---\n",
    );
    write(&home.join("commands/notes.txt"), "ignored");
    write(
        &ws.join(".genie/commands/review.md"),
        "---\ndescription: Project review\nmodel: fast\n---\nProject review of $1",
    );

    let (cmds, diags) = load_commands(&command_dirs(&home, Some(&ws), false));
    let names: Vec<_> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["git:pr", "review"]);
    assert_eq!(cmds[0].description, "Open a PR");
    assert_eq!(cmds[1].description, "Review code");
    assert_eq!(cmds[1].scope, CommandScope::User);
    assert!(diags.iter().any(|d| d.message.contains("built-in")));
    assert!(diags.iter().any(|d| d.path.ends_with("bad.md")));

    // Trusted: the project command wins by name.
    let (cmds, _) = load_commands(&command_dirs(&home, Some(&ws), true));
    let review = cmds.iter().find(|c| c.name == "review").unwrap();
    assert_eq!(review.scope, CommandScope::Project);
    assert_eq!(review.model.as_deref(), Some("fast"));
    assert_eq!(
        review.expand("main.rs extra").text,
        "Project review of main.rs extra"
    );
}
