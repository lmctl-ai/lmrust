//! Port of lmts `tests/interpret.test.ts`: tokenizer, interpreter, classifier and
//! prompt describer.

use lmrust::{
    classify_operation, describe_for_prompt, interpret_command, tokenize_simple_command, Access, Effect, Interpreted,
    IssueCode, OperationSchema, Outcome, ParamSpec, PublishedCliSchema, ValidationIssue,
};
use serde_json::{json, Value};

const GIT2_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/git2.cli.schema.v1.json"
));

fn git2() -> PublishedCliSchema {
    serde_json::from_str(GIT2_FIXTURE).expect("git2 fixture deserializes")
}

fn op(v: Value) -> OperationSchema {
    serde_json::from_value(v).expect("valid OperationSchema literal")
}

fn sample_op_read() -> OperationSchema {
    op(json!({
        "name": "inspect",
        "access": "read",
        "description": "Inspect resource",
        "params": [
            { "name": "id", "type": "string", "occurs": "1", "positional": true, "description": "Resource ID" },
            { "name": "verbose", "type": "boolean", "occurs": "?", "description": "Verbose mode" }
        ]
    }))
}

fn sample_op_write() -> OperationSchema {
    op(json!({
        "name": "create",
        "access": "write",
        "description": "Create resource",
        "params": [
            { "name": "name", "type": "string", "occurs": "1", "description": "Resource name" },
            { "name": "count", "type": "number", "occurs": "?", "default": 1, "description": "Count" },
            { "name": "tags", "type": "array", "items": "string", "occurs": "*", "description": "Tags" }
        ]
    }))
}

fn sample_op_dynamic() -> OperationSchema {
    op(json!({ "name": "sync", "access": "dynamic", "description": "Sync resources", "params": [] }))
}

fn sample_op_missing_access() -> OperationSchema {
    op(json!({ "name": "unspecified", "description": "Op without access field", "params": [] }))
}

fn sample_tool_schema() -> PublishedCliSchema {
    PublishedCliSchema {
        schema: "cli.schema.v1".into(),
        tool: "mytool".into(),
        version: Some("1.0.0".into()),
        policy: Some(lmrust::PolicyFlags {
            dryrun: true,
            readonly: true,
        }),
        inputs: Some(vec![
            lmrust::InputForm::Flags,
            lmrust::InputForm::Json,
            lmrust::InputForm::Graphql,
        ]),
        globals: Some(
            serde_json::from_value(json!([
                { "name": "dryrun", "type": "boolean", "occurs": "?", "aliases": ["dry-run"] },
                { "name": "confirm", "type": "boolean", "occurs": "?" }
            ]))
            .unwrap(),
        ),
        operations: vec![
            sample_op_read(),
            sample_op_write(),
            sample_op_dynamic(),
            sample_op_missing_access(),
        ],
        warnings: None,
        product: None,
    }
}

fn ok<T: std::fmt::Debug>(r: Outcome<T>) -> T {
    match r {
        Ok(v) => v,
        Err(issues) => panic!("expected ok, got {issues:#?}"),
    }
}

fn fail<T: std::fmt::Debug>(r: Outcome<T>) -> Vec<ValidationIssue> {
    match r {
        Ok(v) => panic!("expected failure, got {v:?}"),
        Err(issues) => issues,
    }
}

fn tokens(line: &str) -> Vec<String> {
    ok(tokenize_simple_command(line))
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// First issue is SHELL_SYNTAX and its message contains `needle`.
fn refused(line: &str, needle: &str) {
    let issues = fail(tokenize_simple_command(line));
    assert_eq!(issues[0].code, IssueCode::ShellSyntax, "{line:?}");
    assert!(
        issues[0].message.contains(needle),
        "{line:?}: {:?} does not contain {needle:?}",
        issues[0].message
    );
}

fn interpret_one(schema: &PublishedCliSchema, line: &str) -> Outcome<Interpreted> {
    interpret_command(std::slice::from_ref(schema), line, &[])
}

// ---------------------------------------------------------------------------
// tokenizeSimpleCommand
// ---------------------------------------------------------------------------

#[test]
fn tokenizes_simple_whitespace_separated_words() {
    assert_eq!(
        tokens("git2 commit --message hello"),
        strs(&["git2", "commit", "--message", "hello"])
    );
}

#[test]
fn single_quotes_preserve_content_literally_including_shell_characters() {
    assert_eq!(
        tokens("git2 commit --message 'hello world; && | > < $VAR `cmd`'"),
        strs(&["git2", "commit", "--message", "hello world; && | > < $VAR `cmd`"])
    );
}

#[test]
fn double_quotes_preserve_spaces_and_regular_characters() {
    assert_eq!(
        tokens("git2 commit --message \"hello world\""),
        strs(&["git2", "commit", "--message", "hello world"])
    );
}

#[test]
fn double_quotes_allow_escaping_only_of_quote_and_backslash() {
    assert_eq!(
        tokens(r#"git2 commit --message "hello \"world\" and \\ path""#),
        strs(&["git2", "commit", "--message", r#"hello "world" and \ path"#])
    );
}

#[test]
fn backslash_escapes_space_outside_quotes() {
    assert_eq!(
        tokens(r"git2 commit --message hello\ world"),
        strs(&["git2", "commit", "--message", "hello world"])
    );
}

#[test]
fn backslash_escapes_quotes_and_backslashes_outside_quotes() {
    assert_eq!(
        tokens(r#"git2 commit --message \"hello\"\\world"#),
        strs(&["git2", "commit", "--message", r#""hello"\world"#])
    );
}

#[test]
fn empty_string_argument_in_double_quotes_produces_empty_string_token() {
    assert_eq!(
        tokens("git2 commit --message \"\""),
        strs(&["git2", "commit", "--message", ""])
    );
}

#[test]
fn empty_string_argument_in_single_quotes_produces_empty_string_token() {
    assert_eq!(
        tokens("git2 commit --message ''"),
        strs(&["git2", "commit", "--message", ""])
    );
}

#[test]
fn multiple_empty_string_arguments_are_preserved() {
    assert_eq!(tokens("git2 test \"\" ''"), strs(&["git2", "test", "", ""]));
}

#[test]
fn eq_in_flag_value_outside_quotes_works_as_a_single_token() {
    assert_eq!(
        tokens("git2 commit --message=hello"),
        strs(&["git2", "commit", "--message=hello"])
    );
}

#[test]
fn eq_with_quoted_value_works() {
    assert_eq!(
        tokens("git2 commit --message=\"key=value with spaces\""),
        strs(&["git2", "commit", "--message=key=value with spaces"])
    );
}

#[test]
fn unicode_characters_and_emojis_are_preserved() {
    assert_eq!(
        tokens("git2 commit --message \"コミット 🚀 日本語\""),
        strs(&["git2", "commit", "--message", "コミット 🚀 日本語"])
    );
}

#[test]
fn empty_input_returns_empty_token_array() {
    assert_eq!(tokens(""), Vec::<String>::new());
    assert_eq!(tokens("    \t   "), Vec::<String>::new());
}

#[test]
fn adjacent_quotes_and_unquoted_parts_concatenate_into_one_token() {
    assert_eq!(
        tokens("git2 commit --message=\"hello \"'world'"),
        strs(&["git2", "commit", "--message=hello world"])
    );
}

#[test]
fn hash_in_middle_of_word_is_not_treated_as_a_comment() {
    assert_eq!(
        tokens("git2 commit --message issue#123"),
        strs(&["git2", "commit", "--message", "issue#123"])
    );
}

#[test]
fn tilde_in_middle_of_word_is_not_treated_as_tilde_expansion() {
    assert_eq!(tokens("git2 lm_diff HEAD~1"), strs(&["git2", "lm_diff", "HEAD~1"]));
}

#[test]
fn refuses_unquoted_semicolon() {
    refused("git2 lm_status; rm -rf /", ";");
}

#[test]
fn refuses_unquoted_ampersand() {
    refused("git2 lm_status &", "&");
}

#[test]
fn refuses_unquoted_double_ampersand() {
    refused("git2 lm_status && curl x", "&&");
}

#[test]
fn refuses_unquoted_pipe() {
    refused("git2 lm_status | sh", "|");
}

#[test]
fn refuses_unquoted_double_pipe() {
    refused("git2 lm_status || echo fail", "||");
}

#[test]
fn refuses_unquoted_command_substitution() {
    refused("git2 lm_status $(id)", "$");
}

#[test]
fn refuses_unquoted_variable_expansion() {
    refused("git2 lm_status $HOME", "$");
}

#[test]
fn refuses_unquoted_backticks() {
    refused("git2 lm_status `id`", "`");
}

#[test]
fn refuses_unquoted_redirection() {
    refused("git2 lm_status > /etc/passwd", ">");
}

#[test]
fn refuses_unquoted_append_redirection() {
    refused("git2 lm_status >> out.txt", ">>");
}

#[test]
fn refuses_unquoted_input_redirection() {
    refused("git2 lm_status < /dev/null", "<");
}

#[test]
fn refuses_unquoted_heredoc_redirection() {
    refused("git2 lm_status << EOF", "<<");
}

#[test]
fn refuses_unquoted_parentheses() {
    refused("git2 (subshell)", "(");
}

#[test]
fn refuses_unquoted_braces() {
    refused("git2 { echo hi; }", "{");
}

#[test]
fn refuses_leading_env_assignment() {
    refused("FOO=1 git2 lm_status", "FOO=1");
}

#[test]
fn refuses_newline_in_command() {
    refused("git2 lm_status\nrm x", "newline");
}

#[test]
fn refuses_carriage_return_in_command() {
    refused("git2 lm_status\rrm x", "newline");
}

#[test]
fn refuses_nul_byte_in_command() {
    let issues = fail(tokenize_simple_command("git2 lm_status\0rm x"));
    assert_eq!(issues[0].code, IssueCode::ShellSyntax);
}

#[test]
fn refuses_unquoted_glob_star() {
    refused("git2 lm_diff *.ts", "*");
}

#[test]
fn refuses_unquoted_glob_question() {
    refused("git2 lm_diff file?.ts", "?");
}

#[test]
fn refuses_unquoted_glob_bracket() {
    refused("git2 lm_diff file[0-9].ts", "[");
}

#[test]
fn refuses_word_starting_with_unquoted_tilde() {
    refused("git2 lm_log ~", "~");
    refused("git2 lm_log ~/repo", "~");
}

#[test]
fn refuses_shell_comment_at_word_start() {
    refused("git2 lm_status # a comment", "#");
}

#[test]
fn refuses_shell_comment_at_beginning_of_line() {
    refused("# just a comment", "#");
}

#[test]
fn refuses_unterminated_single_quote() {
    refused("git2 commit --message 'unterminated", "unterminated single quote");
}

#[test]
fn refuses_unterminated_double_quote() {
    refused("git2 commit --message \"unterminated", "unterminated double quote");
}

#[test]
fn refuses_variable_expansion_inside_double_quotes() {
    refused("git2 commit --message \"$FOO\"", "$");
}

#[test]
fn refuses_command_substitution_inside_double_quotes() {
    refused("git2 commit --message \"$(whoami)\"", "$(...)");
}

#[test]
fn refuses_backtick_substitution_inside_double_quotes() {
    refused("git2 commit --message \"`whoami`\"", "`");
}

// ---------------------------------------------------------------------------
// interpretCommand
// ---------------------------------------------------------------------------

#[test]
fn rejects_unknown_tool_with_edit_distance_suggestion() {
    let issues = fail(interpret_command(
        &[git2(), sample_tool_schema()],
        "gti2 lm_status",
        &[],
    ));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::UnknownTool);
    assert_eq!(i.suggestion.as_deref(), Some("git2"));
    assert!(i.message.contains("Available tools:"), "{}", i.message);
    assert!(!i.synopsis.is_empty());
    assert!(!i.usage.is_empty());
}

#[test]
fn rejects_unknown_tool_without_suggestion_when_distance_gt_2() {
    let issues = fail(interpret_command(&[git2()], "foobar lm_status", &[]));
    assert_eq!(issues[0].code, IssueCode::UnknownTool);
    assert_eq!(issues[0].suggestion, None);
}

#[test]
fn rejects_missing_operation_after_tool() {
    let issues = fail(interpret_one(&git2(), "git2"));
    assert_eq!(issues[0].code, IssueCode::UnknownOperation);
    assert!(issues[0].message.contains("missing operation"), "{}", issues[0].message);
}

#[test]
fn rejects_flag_immediately_after_tool_with_flag_before_op() {
    let issues = fail(interpret_one(&git2(), "git2 --schema"));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::FlagBeforeOp);
    assert!(i.message.contains("--schema"), "{}", i.message);
    assert!(!i.synopsis.is_empty());
    assert!(!i.usage.is_empty());
}

#[test]
fn rejects_unknown_operation_with_edit_distance_suggestion() {
    let issues = fail(interpret_one(&git2(), "git2 lm_statux"));
    let i = &issues[0];
    assert_eq!(i.code, IssueCode::UnknownOperation);
    assert_eq!(i.suggestion.as_deref(), Some("lm_status"));
    assert!(i.message.contains("Available operations:"), "{}", i.message);
}

#[test]
fn parses_and_validates_typed_params_and_positionals_into_interpreted() {
    let v = ok(interpret_one(&sample_tool_schema(), "mytool inspect res123 --verbose"));
    assert_eq!(v.tool, "mytool");
    assert_eq!(v.op, "inspect");
    assert_eq!(v.argv, strs(&["mytool", "inspect", "res123", "--verbose"]));
    assert_eq!(v.positionals, strs(&["res123"]));
    assert_eq!(Value::Object(v.params), json!({ "id": "res123", "verbose": true }));
    assert_eq!(v.effect, Effect::Read);
    assert_eq!(v.access, Some(Access::Read));
}

#[test]
fn extracts_globals_without_polluting_params() {
    let extra: Vec<ParamSpec> = serde_json::from_value(json!([
        { "name": "dryrun", "type": "boolean", "occurs": "?" },
        { "name": "confirm", "type": "boolean", "occurs": "?" }
    ]))
    .unwrap();
    let v = ok(interpret_command(
        &[sample_tool_schema()],
        "mytool create --name myres --dryrun --confirm",
        &extra,
    ));
    assert_eq!(Value::Object(v.globals), json!({ "dryrun": true, "confirm": true }));
    assert!(!v.params.contains_key("dryrun"));
    assert!(!v.params.contains_key("confirm"));
    assert_eq!(v.params["name"], json!("myres"));
    assert_eq!(v.params["count"], json!(1)); // default applied
    assert_eq!(v.effect, Effect::Effectful);
    assert_eq!(v.access, Some(Access::Write));
}

#[test]
fn collects_duplicate_flags_as_issues() {
    let issues = fail(interpret_one(&sample_tool_schema(), "mytool create --name a --name b"));
    assert!(issues.iter().any(|i| i.code == IssueCode::DuplicateFlag), "{issues:#?}");
}

#[test]
fn collects_missing_required_parameter_issue() {
    let issues = fail(interpret_one(&sample_tool_schema(), "mytool create"));
    let missing = issues
        .iter()
        .find(|i| i.code == IssueCode::MissingRequired)
        .expect("MISSING_REQUIRED issue");
    assert_eq!(missing.param.as_deref(), Some("name"));
}

#[test]
fn all_issues_always_carry_non_empty_synopsis_and_usage() {
    let issues = fail(interpret_one(&sample_tool_schema(), "mytool create --unknown-flag"));
    assert!(!issues.is_empty());
    for i in &issues {
        assert!(!i.synopsis.is_empty(), "{i:?}");
        assert!(!i.usage.is_empty(), "{i:?}");
    }
}

// TS: "null-prototype safe records in Interpreted" checks Object.getPrototypeOf(...) === null,
// which has no Rust equivalent. The behaviour behind it: params/globals hold exactly the
// parsed keys, and reserved names never reach them.
#[test]
fn safe_records_in_interpreted() {
    let v = ok(interpret_one(&sample_tool_schema(), "mytool inspect res123"));
    assert_eq!(Value::Object(v.params), json!({ "id": "res123" }));
    assert_eq!(Value::Object(v.globals), json!({}));
    for flag in ["--__proto__=x", "--constructor=x", "--prototype=x"] {
        let line = format!("mytool inspect res123 {flag}");
        let issues = fail(interpret_one(&sample_tool_schema(), &line));
        assert!(
            issues.iter().any(|i| i.code == IssueCode::UnknownParam),
            "{flag}: {issues:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// classifyOperation
// ---------------------------------------------------------------------------

#[test]
fn read_access_yields_read() {
    assert_eq!(classify_operation(&sample_op_read()), Effect::Read);
}

#[test]
fn write_access_yields_effectful() {
    assert_eq!(classify_operation(&sample_op_write()), Effect::Effectful);
}

#[test]
fn dynamic_access_yields_effectful() {
    assert_eq!(classify_operation(&sample_op_dynamic()), Effect::Effectful);
}

#[test]
fn missing_access_yields_effectful() {
    assert_eq!(classify_operation(&sample_op_missing_access()), Effect::Effectful);
}

#[test]
fn schema_cannot_upgrade_itself_to_read_only() {
    let dyn_op = op(json!({ "name": "safe_looking", "access": "dynamic", "params": [] }));
    assert_eq!(classify_operation(&dyn_op), Effect::Effectful);
}

// ---------------------------------------------------------------------------
// describeForPrompt
// ---------------------------------------------------------------------------

const GIT2_GOLDEN: &str = concat!(
    "# git2\n",
    "git2 add [--all] [--force] [--path <string>] [--paths <string>...] [--update] (effectful) — Stage files\n",
    "git2 blame [--end-line <number>] --file <string> [--ignore-whitespace] [--path <string>] [--start-line <number>] (read) — Blame a file\n",
    "git2 branch_create --branch-name <string> [--force] [--path <string>] [--start-point <string>] (effectful) — Create a branch\n",
    "git2 branch_current [--path <string>] (read) — Show current branch\n",
    "git2 branch_delete --branch-name <string> [--force] [--path <string>] (effectful) — Delete a branch\n",
    "git2 branch_list [--all] [--limit <number>] [--merged] [--no-merged] [--path <string>] [--remote] (read) — List branches\n",
    "git2 branch_rename --branch-name <string> [--force] --new-branch-name <string> [--path <string>] (effectful) — Rename a branch\n",
    "git2 checkout <target> [--create-branch] [--force] [--path <string>] [--paths <string>...] [--track] (effectful) — Checkout a branch/commit/paths\n",
    "git2 cherry_pick --commits <string>... [--mainline <number>] [--no-commit] [--path <string>] [--signoff] [--strategy <string>] (effectful) — Apply commits on top\n",
    "git2 clean [--directories] [--dry-run] [--force] [--ignored] [--path <string>] (effectful) — Remove untracked files / directories\n",
    "git2 clone [--bare] [--branch <string>] [--depth <number>] --local-path <string> [--mirror] --remote-url <string> (effectful) — Clone a remote repository\n",
    "git2 commit [--allow-empty] [--amend] [--author <object>] --message <string> [--no-verify] [--path <string>] (effectful) — Create a commit\n",
    "git2 diff [--commit1 <string>] [--commit2 <string>] [--exclude-patterns <string>...] [--include-untracked] [--name-only] [--path <string>] [--paths <string>...] [--staged] [--stat] [--unified <number>] (read) — Show diffs\n",
    "git2 fetch [--depth <number>] [--path <string>] [--prune] [--remote <string>] [--tags] (effectful) — Fetch from a remote\n",
    "git2 init [--bare] [--initial-branch <string>] [--path <string>] (effectful) — Initialize a new git repository\n",
    "git2 lm_blame --file <string> [--path <string>] [--revision <string>] (read) — Per-line blame with author metadata\n",
    "git2 lm_branch [--all] [--path <string>] (read) — Branches with upstream and ahead/behind\n",
    "git2 lm_diff [--from <string>] [--path <string>] [--paths <string>...] [--staged] [--to <string>] (read) — Structured unified diff with hunks and byte-preserving content\n",
    "git2 lm_log [--max-count <number>] [--path <string>] [--revision <string>] (read) — Structured commit log with author/committer metadata\n",
    "git2 lm_ls_files [--path <string>] [--paths <string>...] (read) — List index entries with mode/stage\n",
    "git2 lm_remote [--path <string>] (read) — Remotes with credentials redacted from URLs\n",
    "git2 lm_rev_parse [--path <string>] [--revision <string>] (read) — Resolve a revision and peel to commit\n",
    "git2 lm_show [--file <string>] [--path <string>] [--revision <string>] (read) — Show a commit with metadata, or a file from its tree\n",
    "git2 lm_status [--ignored] [--path <string>] (read) — Structured git status (porcelain v1, byte-preserving paths)\n",
    "git2 log [--author <string>] [--branch <string>] [--file <string>] [--follow] [--grep <string>] [--max-count <number>] [--path <string>] [--since <string>] [--skip <number>] [--until <string>] (read) — Commit history\n",
    "git2 merge [--abort] --branch <string> [--message <string>] [--no-fast-forward] [--path <string>] [--squash] [--strategy <string>] (effectful) — Merge a branch\n",
    "git2 pull [--branch <string>] [--ff-only] [--path <string>] [--rebase] [--remote <string>] (effectful) — Pull from a remote\n",
    "git2 push [--branch <string>] [--delete] [--dry-run] [--force] [--force-with-lease] [--path <string>] [--remote <string>] [--set-upstream] [--tags] (effectful) — Push to a remote\n",
    "git2 rebase [--abort] --branch <string> [--continue-op] [--interactive] [--onto <string>] [--path <string>] [--preserve-merges] (effectful) — Rebase current branch\n",
    "git2 reflog [--max-count <number>] [--path <string>] [--ref <string>] (read) — Show reflog\n",
    "git2 remote_add --name <string> [--path <string>] --url <string> (effectful) — Add a remote\n",
    "git2 remote_get_url --name <string> [--path <string>] (read) — Get remote URL\n",
    "git2 remote_list [--path <string>] (read) — List remotes\n",
    "git2 remote_remove --name <string> [--path <string>] (effectful) — Remove a remote\n",
    "git2 remote_set_url --name <string> [--path <string>] [--push] --url <string> (effectful) — Set remote URL\n",
    "git2 reset [--commit <string>] [--mode <soft|mixed|hard|merge|keep>] [--path <string>] [--paths <string>...] (effectful) — Reset HEAD / unstage paths\n",
    "git2 show [--file-path <string>] [--format <string>] --object <string> [--path <string>] [--stat] (read) — Show a git object (commit, tree, blob, tag)\n",
    "git2 stash_apply [--path <string>] (effectful) — Apply a stash\n",
    "git2 stash_clear [--path <string>] (effectful) — Clear all stashes\n",
    "git2 stash_drop [--path <string>] [--stash-ref <string>] (effectful) — Drop a stash\n",
    "git2 stash_list [--limit <number>] [--path <string>] (read) — List stashes\n",
    "git2 stash_pop [--path <string>] (effectful) — Pop a stash\n",
    "git2 stash_push [--include-untracked] [--keep-index] [--message <string>] [--path <string>] (effectful) — Stash changes\n",
    "git2 status [--ignore-submodules] [--include-untracked] [--path <string>] (read) — Show working tree status\n",
    "git2 tag_create [--annotated] [--commit <string>] [--force] [--message <string>] [--path <string>] --tag-name <string> (effectful) — Create a tag\n",
    "git2 tag_delete [--path <string>] --tag-name <string> (effectful) — Delete a tag\n",
    "git2 tag_list [--limit <number>] [--path <string>] (read) — List tags\n",
    "git2 tag_verify [--path <string>] --tag-name <string> (read) — Verify a tag\n",
    "git2 worktree_add [--branch <string>] [--commitish <string>] [--detach] [--force] --path <string> [--repo <string>] (effectful) — Add a worktree\n",
    "git2 worktree_list [--repo <string>] (read) — List worktrees\n",
    "git2 worktree_prune [--dry-run] [--repo <string>] [--verbose] (effectful) — Prune stale worktrees\n",
    "git2 worktree_remove [--force] --path <string> [--repo <string>] (effectful) — Remove a worktree\n",
    "\n",
    "Rules:\n",
    "1. One simple command: execute exactly one command line starting with the tool name; do not chain commands.\n",
    "2. Quote values with spaces: wrap parameter values containing spaces or special characters in quotes.\n",
    "3. No shell operators: operators, pipes, redirects, subshells, backticks, globs, and env prefixes are refused.",
);

#[test]
fn matches_golden_rendering_of_git2_fixture() {
    let rendered = describe_for_prompt(&[git2()]);
    assert!(rendered.starts_with("# git2"));
    for needle in [
        "git2 commit [--allow-empty] [--amend] [--author <object>] --message <string> [--no-verify] [--path <string>] (effectful) — Create a commit",
        "git2 cherry_pick --commits <string>... [--mainline <number>] [--no-commit] [--path <string>] [--signoff] [--strategy <string>] (effectful) — Apply commits on top",
        "git2 status [--ignore-submodules] [--include-untracked] [--path <string>] (read) — Show working tree status",
        "Rules:",
        "1. One simple command:",
        "2. Quote values with spaces:",
        "3. No shell operators:",
    ] {
        assert!(rendered.contains(needle), "missing {needle:?}");
    }
    assert_eq!(rendered, GIT2_GOLDEN);
}

// ---------------------------------------------------------------------------
// property-based security invariant test
// ---------------------------------------------------------------------------

/// Mulberry32 seeded PRNG, bit-exact with the TS test.
struct Mulberry32(u32);

impl Mulberry32 {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

const REFUSED: &str = ";&|<>(){}*?[\n\r\0`$";

fn has_unquoted_refusal(s: &str) -> bool {
    if s.contains(['\r', '\n', '\0']) {
        return true;
    }
    #[derive(PartialEq)]
    enum Mode {
        Normal,
        Single,
        Double,
    }
    let chars: Vec<char> = s.chars().collect();
    let mut mode = Mode::Normal;
    let mut is_word_start = true;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        match mode {
            Mode::Single => {
                if ch == '\'' {
                    mode = Mode::Normal;
                }
                i += 1;
                continue;
            }
            Mode::Double => {
                if ch == '"' {
                    mode = Mode::Normal;
                } else if ch == '$' || ch == '`' {
                    return true;
                } else if ch == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                i += 1;
                continue;
            }
            Mode::Normal => {}
        }
        if ch == ' ' || ch == '\t' {
            is_word_start = true;
            i += 1;
            continue;
        }
        if is_word_start && (ch == '#' || ch == '~') {
            return true;
        }
        if ch == '\'' || ch == '"' {
            mode = if ch == '\'' { Mode::Single } else { Mode::Double };
            is_word_start = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            is_word_start = false;
            if i + 1 < chars.len() {
                if REFUSED.contains(chars[i + 1]) {
                    return true;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if REFUSED.contains(ch) {
            return true;
        }
        is_word_start = false;
        i += 1;
    }
    mode != Mode::Normal // unterminated quote
}

#[test]
fn random_strings_never_panic_and_never_ok_when_containing_unquoted_refused_characters() {
    let mut rand = Mulberry32(1337);
    let safe_chars = "abcdefghijklmnopqrstuvwxyz0123456789-_=./: ";
    let quotes = "'\"";
    let dangerous_chars = ";&|<>(){}*?[\n\r\0`$#~";
    let all_chars: Vec<char> = format!("{safe_chars}{quotes}{dangerous_chars}").chars().collect();
    let schemas = [git2()];
    let mut refused_count = 0;
    for _ in 0..2000 {
        let len = (rand.next() * 40.0).floor() as usize + 1;
        let mut s = String::new();
        for _ in 0..len {
            let idx = (rand.next() * all_chars.len() as f64).floor() as usize;
            s.push(all_chars[idx]);
        }
        let result = std::panic::catch_unwind(|| interpret_command(&schemas, &s, &[]));
        let result = result.unwrap_or_else(|_| panic!("interpret_command panicked on {s:?}"));
        if has_unquoted_refusal(&s) {
            refused_count += 1;
            assert!(
                result.is_err(),
                "{s:?} contains an unquoted refused character but was accepted"
            );
        }
    }
    assert!(refused_count > 0);
}

/// Guards the PRNG port: first outputs of the TS Mulberry32(1337).
#[test]
fn mulberry32_matches_js_reference() {
    let mut r = Mulberry32(1337);
    assert_eq!(r.next(), 0.1844118325971067);
    assert_eq!(r.next(), 0.18998925131745636);
    assert_eq!(r.next(), 0.8104719922412187);
}
