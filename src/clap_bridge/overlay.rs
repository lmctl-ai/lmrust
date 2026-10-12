//! The overlay: the hand-maintained part of a bridged tool's lmapi surface (published op names,
//! access, dangerous ops, parameter exclusions, the legacy JSON switch, stdin params, exit-code
//! mapping and output caps). Everything else is generated from clap.

use crate::server::ErrorCode;
use crate::types::InputForm;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A parsed and checked overlay document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    /// Published tool name (default: the clap command's name).
    pub tool: Option<String>,
    /// Tool description for `--help` (default: the clap command's `about`).
    pub description: Option<String>,
    /// Published input forms (default `["flags", "json"]`).
    #[serde(default = "default_inputs")]
    pub inputs: Vec<InputForm>,
    /// lmapi runner globals the tool owns and that are therefore not published (e.g. `format`).
    #[serde(default)]
    pub drop_globals: Vec<String>,
    /// Clap command paths (space-separated) that are never published; a first word excludes
    /// the whole group.
    #[serde(default)]
    pub excluded: Vec<String>,
    /// Clap arg ids never published for any op.
    #[serde(default)]
    pub exclude_params: Vec<String>,
    /// Clap arg ids published only for the listed ops.
    #[serde(default)]
    pub only_for: BTreeMap<String, Vec<String>>,
    /// The tool's own JSON switch (`["--json"]`, `["--format", "json"]`), appended to every
    /// self-exec call after the flags and before the positionals.
    #[serde(default)]
    pub legacy_json_args: Vec<String>,
    /// Arguments placed before the command words of every self-exec call (root-only options).
    #[serde(default)]
    pub legacy_root_args: Vec<String>,
    /// Key of the result object when the child's stdout is not JSON (default `"text"`).
    #[serde(default = "default_text_key")]
    pub text_key: String,
    /// Child exit codes that are successful results (default `[0]`).
    #[serde(default = "default_success")]
    pub success_exit_codes: Vec<i32>,
    /// Child exit code (as a string key) to lmapi error code, e.g. `"4" = "NOT_FOUND"`.
    /// Unlisted failures are `COMMAND_FAILED` (`INTERNAL_ERROR` for a Rust panic, 101).
    #[serde(default)]
    pub exit_codes: BTreeMap<String, String>,
    /// Bytes of child stdout kept; more is an `OUTPUT_TOO_LARGE` error (default 16 MiB).
    #[serde(default = "default_max_stdout")]
    pub max_stdout_bytes: usize,
    /// Bytes of child stderr kept; the rest is dropped (default 64 KiB).
    #[serde(default = "default_max_stderr")]
    pub max_stderr_bytes: usize,
    /// Kill the child after this many milliseconds (`TIMEOUT`, exit 7). Default: no deadline.
    pub timeout_ms: Option<u64>,
    /// Publish clap positionals as named params (no `positional: true`): library-style calls
    /// pass every value by name. The legacy argv still places them positionally.
    #[serde(default)]
    pub named_positionals: bool,
    /// Inline alternatives for a file-like input, by published param name (e.g. `file`): every
    /// op with that param also accepts the listed forms, piped to stdin (see [`InlineInput`]).
    #[serde(default)]
    pub inline_inputs: BTreeMap<String, InlineInput>,
    #[serde(rename = "op", default)]
    pub ops: Vec<OverlayOp>,
}

/// Declared access of an overlay op. There is no default: a missing `access` is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayAccess {
    Read,
    Write,
    /// Decided per call by [`super::Bridge::dynamic_access`]; without a hook it is a write.
    Dynamic,
}

/// One published operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayOp {
    /// Published name (`[a-z][a-z0-9_]*`); never equal to a legacy subcommand name.
    pub name: String,
    /// Clap command path, words separated by spaces (`data load`).
    pub command: String,
    pub access: OverlayAccess,
    /// Requires `--confirm` (exit 9 without it). Only for write or dynamic ops.
    #[serde(default)]
    pub dangerous: bool,
    /// Replaces the clap `about` text.
    pub description: Option<String>,
    /// Clap arg ids not published for this op.
    #[serde(default)]
    pub exclude_params: Vec<String>,
    /// Legacy arguments always passed (after the flags).
    #[serde(default)]
    pub fixed_args: Vec<String>,
    /// `false`: none of the root command's own args is published for this op.
    #[serde(default = "yes")]
    pub common_options: bool,
    /// `false`: do not append `legacy_json_args` (a command without the JSON switch).
    #[serde(default = "yes")]
    pub legacy_json: bool,
    /// Per-param settings, by published (camelCase) name.
    #[serde(default)]
    pub params: BTreeMap<String, OverlayParam>,
    /// The command writes its data to a file: the bridge passes a temp path and returns the file.
    pub result_file: Option<ResultFile>,
}

/// An op whose data is written to a file named by one of its args (`--output-path`). The arg is not
/// published; every call gets `<temp dir>/<name>` (or the temp dir itself when `name` is absent),
/// and after a successful run `<temp dir>/<read or name>` is parsed as the result. The temp dir and
/// everything the command wrote into it are removed afterwards.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultFile {
    /// Published (camelCase) name of the output-path arg.
    pub param: String,
    /// File name passed to the command (its extension may select the format); absent: the dir.
    pub name: Option<String>,
    /// The file to read as the result, when it is not `name` (a sidecar such as `x.summary.json`).
    pub read: Option<String>,
    pub format: ResultFormat,
}

/// How a result file is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultFormat {
    /// RFC 4180 with a header row: an array of row objects; cells typed as number, boolean, null
    /// (empty) or string.
    Csv,
    Json,
    /// The file text under the overlay's `text_key`.
    Text,
}

/// Settings of one op param.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayParam {
    /// Pipe the value to the child's stdin instead of placing it in argv. A clap-backed param
    /// gets `stdin_token` (default `-`) in its argv slot; a param that is not a clap arg is
    /// added to the schema (it then needs `description`).
    #[serde(default)]
    pub stdin: bool,
    /// What the argv slot of a stdin param receives (default `-`; `""` leaves the slot out).
    pub stdin_token: Option<String>,
    /// Replaces the clap help text (required for a param that is not a clap arg).
    pub description: Option<String>,
    /// A param that is not a clap arg: an array of strings (occurs `+`), one line each on stdin.
    #[serde(default)]
    pub list: bool,
}

/// A file-like input that may instead be passed inline. The input param becomes optional; a call
/// gives exactly one of the input or one of the forms. A form's value is piped to the child's
/// stdin, the input's argv slot gets `token`, and the form's `args` are added (after the fixed
/// args, before the JSON switch), e.g. `--from json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineInput {
    /// What the input's argv slot receives (default `-`).
    #[serde(default = "dash")]
    pub token: String,
    /// Description of the input param, used when the clap arg has no help text.
    pub description: Option<String>,
    #[serde(rename = "form", default)]
    pub forms: Vec<InlineForm>,
}

/// One inline form of an [`InlineInput`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineForm {
    /// Published param name (camelCase), added to every op that has the input.
    pub param: String,
    pub kind: InlineKind,
    /// Legacy args telling the tool the stdin format (e.g. `["--from", "json"]`).
    #[serde(default)]
    pub args: Vec<String>,
    pub description: Option<String>,
    /// How the value reaches the command: piped to stdin (default) or written to a temp file
    /// whose path takes the input's argv slot (a tool that only reads config files).
    #[serde(default)]
    pub via: InlineVia,
    /// Temp file name for `via = "file"` (its extension may select the format, e.g. `config.toml`).
    pub file_name: Option<String>,
    /// Published op names this form applies to (default: every op with the input).
    pub ops: Option<Vec<String>>,
}

/// Delivery of an inline value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InlineVia {
    #[default]
    Stdin,
    File,
}

/// How an inline value is published and written to stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InlineKind {
    /// An array of objects (table rows), written as one compact JSON array.
    Records,
    /// A string (CSV, TSV, ...), written as is.
    Text,
    /// Any JSON value, written as compact JSON.
    Json,
    /// A JSON object written as TOML (no nulls).
    Toml,
}

fn dash() -> String {
    "-".into()
}

fn yes() -> bool {
    true
}

fn default_inputs() -> Vec<InputForm> {
    vec![InputForm::Flags, InputForm::Json]
}

fn default_text_key() -> String {
    "text".into()
}

fn default_success() -> Vec<i32> {
    vec![0]
}

fn default_max_stdout() -> usize {
    16 * 1024 * 1024
}

fn default_max_stderr() -> usize {
    64 * 1024
}

/// A plain file name: no separator, not `.`/`..`, not empty.
fn is_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

/// lmapi identifier rule for published op names: snake_case ASCII.
pub(crate) fn is_op_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Overlay {
    /// Parse and check an overlay document (usually `include_str!("../lmapi/overlay.toml")`).
    pub fn parse(text: &str) -> Result<Overlay, String> {
        let overlay: Overlay = toml::from_str(text).map_err(|e| format!("invalid lmapi overlay: {e}"))?;
        let problems = overlay.problems();
        if problems.is_empty() {
            Ok(overlay)
        } else {
            Err(format!("invalid lmapi overlay: {}", problems.join("; ")))
        }
    }

    /// Rules that need no clap tree.
    pub(crate) fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (code, name) in &self.exit_codes {
            if code.parse::<i32>().is_err() {
                out.push(format!("exit_codes: `{code}` is not an exit code"));
            }
            if ErrorCode::parse(name).is_none() {
                out.push(format!("exit_codes.{code}: unknown lmapi error code {name}"));
            }
        }
        if self.text_key.is_empty() {
            out.push("text_key must not be empty".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for op in &self.ops {
            if !is_op_name(&op.name) {
                out.push(format!("op name {} is not snake_case ([a-z][a-z0-9_]*)", op.name));
            }
            if !seen.insert(op.name.as_str()) {
                out.push(format!("duplicate op name {}", op.name));
            }
            if op.dangerous && op.access == OverlayAccess::Read {
                out.push(format!("op {}: a read op cannot be dangerous", op.name));
            }
            if op.params.values().filter(|p| p.stdin).count() > 1 {
                out.push(format!("op {}: at most one stdin param", op.name));
            }
            if let Some(rf) = &op.result_file {
                for name in rf.name.iter().chain(&rf.read) {
                    if !is_file_name(name) {
                        out.push(format!(
                            "op {}: result_file name {name:?} must be a plain file name",
                            op.name
                        ));
                    }
                }
                if rf.name.is_none() && rf.read.is_none() {
                    out.push(format!(
                        "op {}: result_file passes a directory and needs `read`",
                        op.name
                    ));
                }
            }
        }
        for (input, inline) in &self.inline_inputs {
            if inline.forms.is_empty() {
                out.push(format!("inline_inputs.{input}: no form"));
            }
            let mut names = std::collections::BTreeSet::new();
            for form in &inline.forms {
                if form.param == *input || !names.insert(form.param.as_str()) {
                    out.push(format!("inline_inputs.{input}: duplicate param {}", form.param));
                }
                match (&form.via, &form.file_name) {
                    (InlineVia::File, None) => out.push(format!(
                        "inline_inputs.{input}.{}: via = \"file\" needs file_name",
                        form.param
                    )),
                    (InlineVia::File, Some(name)) if !is_file_name(name) => out.push(format!(
                        "inline_inputs.{input}.{}: file_name must be a plain file name",
                        form.param
                    )),
                    (InlineVia::Stdin, Some(_)) => out.push(format!(
                        "inline_inputs.{input}.{}: file_name needs via = \"file\"",
                        form.param
                    )),
                    _ => {}
                }
                for op in form.ops.iter().flatten() {
                    if !self.ops.iter().any(|o| &o.name == op) {
                        out.push(format!("inline_inputs.{input}.{}: unknown op {op}", form.param));
                    }
                }
            }
        }
        for (arg, ops) in &self.only_for {
            for name in ops {
                if !self.ops.iter().any(|op| &op.name == name) {
                    out.push(format!("only_for.{arg}: unknown op {name}"));
                }
            }
        }
        out
    }

    /// The lmapi error code configured for a child exit code.
    pub(crate) fn mapped_exit(&self, code: i32) -> Option<ErrorCode> {
        self.exit_codes
            .get(&code.to_string())
            .and_then(|name| ErrorCode::parse(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OP: &str = "[[op]]\nname = \"x\"\ncommand = \"x\"\n";

    #[test]
    fn defaults() {
        let o = Overlay::parse(&format!("{OP}access = \"read\"\n")).unwrap();
        assert_eq!(o.inputs, [InputForm::Flags, InputForm::Json]);
        assert_eq!(o.text_key, "text");
        assert_eq!(o.success_exit_codes, [0]);
        assert_eq!(o.max_stdout_bytes, 16 * 1024 * 1024);
        assert!(o.ops[0].legacy_json && o.ops[0].common_options && !o.ops[0].dangerous);
    }

    #[test]
    fn a_missing_access_is_an_error() {
        let err = Overlay::parse(OP).unwrap_err();
        assert!(err.contains("access"), "{err}");
    }

    #[test]
    fn unknown_keys_and_values_are_errors() {
        assert!(Overlay::parse(&format!("{OP}access = \"readonly\"\n")).is_err());
        assert!(Overlay::parse(&format!("{OP}access = \"read\"\ndangrous = true\n")).is_err());
        assert!(Overlay::parse(&format!("legacy_json = [\"--json\"]\n{OP}access = \"read\"\n")).is_err());
        let err = Overlay::parse(&format!("[exit_codes]\n\"x\" = \"NOPE\"\n{OP}access = \"read\"\n")).unwrap_err();
        assert!(
            err.contains("not an exit code") && err.contains("unknown lmapi error code NOPE"),
            "{err}"
        );
    }

    #[test]
    fn inline_input_rules() {
        let o = Overlay::parse(&format!(
            "named_positionals = true\n{OP}access = \"read\"\n[inline_inputs.file]\n[[inline_inputs.file.form]]\n\
             param = \"records\"\nkind = \"records\"\nargs = [\"--from\", \"json\"]\n"
        ))
        .unwrap();
        assert!(o.named_positionals);
        assert_eq!(o.inline_inputs["file"].token, "-");
        assert_eq!(o.inline_inputs["file"].forms[0].kind, InlineKind::Records);
        let err = Overlay::parse(&format!("{OP}access = \"read\"\n[inline_inputs.file]\ntoken = \"-\"\n")).unwrap_err();
        assert!(err.contains("inline_inputs.file: no form"), "{err}");
        let err = Overlay::parse(&format!(
            "{OP}access = \"read\"\n[inline_inputs.file]\n[[inline_inputs.file.form]]\nparam = \"file\"\nkind = \"text\"\n"
        ))
        .unwrap_err();
        assert!(err.contains("duplicate param file"), "{err}");
        assert!(Overlay::parse(&format!(
            "{OP}access = \"read\"\n[inline_inputs.file]\n[[inline_inputs.file.form]]\nparam = \"x\"\nkind = \"rows\"\n"
        ))
        .is_err());
    }

    #[test]
    fn op_rules() {
        let err = Overlay::parse(&format!("{OP}access = \"read\"\ndangerous = true\n")).unwrap_err();
        assert!(err.contains("cannot be dangerous"), "{err}");
        let err = Overlay::parse("[[op]]\nname = \"Bad-Name\"\ncommand = \"x\"\naccess = \"write\"\n").unwrap_err();
        assert!(err.contains("snake_case"), "{err}");
        let err = Overlay::parse(&format!("{OP}access = \"read\"\n{OP}access = \"read\"\n")).unwrap_err();
        assert!(err.contains("duplicate op name x"), "{err}");
        let err = Overlay::parse(&format!(
            "{OP}access = \"read\"\n[op.params.a]\nstdin = true\n[op.params.b]\nstdin = true\n"
        ))
        .unwrap_err();
        assert!(err.contains("at most one stdin param"), "{err}");
        let err = Overlay::parse(&format!("[only_for]\nv = [\"y\"]\n{OP}access = \"read\"\n")).unwrap_err();
        assert!(err.contains("only_for.v: unknown op y"), "{err}");
    }
}
