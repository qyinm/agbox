#![allow(clippy::unwrap_used)]

use agbox_cli::args::Cli;
use clap::Parser;

#[test]
fn parses_the_approved_command_groups_and_rejects_execution_verbs() {
    let cases = [
        &["agbox", "init"][..],
        &["agbox", "init", "--quiet"][..],
        &["agbox", "status"][..],
        &["agbox", "doctor"][..],
        &["agbox", "daemon", "start"][..],
        &["agbox", "agent", "list"][..],
        &["agbox", "work", "current"][..],
        &["agbox", "handoff", "work_1"][..],
        &["agbox", "evidence", "ev_1"][..],
        &["agbox", "search", "sqlite writer"][..],
        &["agbox", "tui"][..],
        &["agbox", "mcp", "--provider", "codex", "--project-root", "."][..],
        &["agbox", "config", "show"][..],
        &["agbox", "forget", "project"][..],
    ];
    for argv in cases {
        assert!(Cli::try_parse_from(argv).is_ok(), "{argv:?}");
    }
    for command in ["run", "assign", "execute"] {
        assert!(Cli::try_parse_from(["agbox", command]).is_err());
    }
}
