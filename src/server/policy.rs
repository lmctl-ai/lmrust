//! Operation policy (lmts `server/policy.ts`): read/write classification, dry-run
//! simulation, read-only enforcement and confirmation of dangerous operations.
//!
//! - Read (and dynamic) operations always pass; the op decides at run time.
//! - Write operations: read-only aborts with `READONLY_VIOLATION`; dry-run returns a
//!   synthetic success result instead of executing; a dangerous op also needs
//!   `--confirm` / `--i-understand` (`MISSING_CONFIRMATION`).

use super::errors::{CliError, ErrorCode};
use crate::types::{Access, Record};
use serde_json::Value;

/// Runtime policy of one invocation (flags and `<TOOL>_DRYRUN` / `<TOOL>_READONLY` floors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Policy {
    /// Write operations are simulated instead of executed.
    pub dry_run: bool,
    /// Write operations abort with `READONLY_VIOLATION`.
    pub read_only: bool,
    /// User attestation (`--confirm` / `--i-understand`).
    pub confirmed: bool,
}

/// What a policy check is about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckContext {
    /// Operation name, used in messages and results.
    pub name: String,
    /// The argv that would run, when already built.
    pub command: Option<Vec<String>>,
    /// Extra fields merged into the dry-run result / error data.
    pub data: Option<Record>,
    /// Destructive or irreversible: requires confirmation.
    pub dangerous: bool,
    /// Per-call confirmation (in addition to the policy's).
    pub confirmed: bool,
}

impl CheckContext {
    pub fn new(name: impl Into<String>) -> Self {
        CheckContext {
            name: name.into(),
            ..CheckContext::default()
        }
    }

    pub fn command<I, S>(mut self, argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.command = Some(argv.into_iter().map(Into::into).collect());
        self
    }

    pub fn dangerous(mut self, dangerous: bool) -> Self {
        self.dangerous = dangerous;
        self
    }

    pub fn data(mut self, data: Record) -> Self {
        self.data = Some(data);
        self
    }
}

fn command_value(command: &Option<Vec<String>>) -> Value {
    match command {
        Some(c) => Value::Array(c.iter().cloned().map(Value::String).collect()),
        None => Value::Null,
    }
}

/// Guard an operation. `Ok(None)`: proceed. `Ok(Some(result))`: a write under dry-run;
/// return this synthetic result (`{success: true, dryRun: true, simulated, ...data, command?}`)
/// instead of executing. `Err`: read-only violation or missing confirmation.
pub fn check_policy(access: Access, policy: &Policy, ctx: &CheckContext) -> Result<Option<Record>, CliError> {
    if matches!(access, Access::Read | Access::Dynamic) {
        return Ok(None);
    }
    let extra = ctx.data.clone().unwrap_or_default();
    if policy.read_only {
        let mut data = Record::new();
        data.insert("operation".into(), Value::String(ctx.name.clone()));
        if ctx.command.is_some() {
            data.insert("command".into(), command_value(&ctx.command));
        }
        data.insert(
            "hint".into(),
            Value::String("Remove the readonly configuration to allow write operations.".into()),
        );
        data.extend(extra);
        return Err(CliError::new(
            ErrorCode::ReadOnlyViolation,
            format!(
                "Operation \"{}\" is a write operation and this tool is running in read-only mode. Aborting.",
                ctx.name
            ),
        )
        .with_data(data));
    }
    if policy.dry_run {
        let mut result = Record::new();
        result.insert("success".into(), Value::Bool(true));
        result.insert("dryRun".into(), Value::Bool(true));
        result.insert("simulated".into(), Value::String(ctx.name.clone()));
        result.extend(extra);
        if ctx.command.is_some() {
            result.insert("command".into(), command_value(&ctx.command));
        }
        return Ok(Some(result));
    }
    if ctx.dangerous && !ctx.confirmed && !policy.confirmed {
        let mut data = Record::new();
        data.insert("operation".into(), Value::String(ctx.name.clone()));
        if ctx.command.is_some() {
            data.insert("command".into(), command_value(&ctx.command));
        }
        data.extend(extra);
        return Err(CliError::new(
            ErrorCode::MissingConfirmation,
            format!(
                "Operation \"{}\" is destructive/irreversible. Re-run with --confirm (or --i-understand) to proceed; use --dryrun to preview the command.",
                ctx.name
            ),
        )
        .with_data(data));
    }
    Ok(None)
}

/// True when a result is a synthetic dry-run result.
pub fn is_dry_run_result(value: &Value) -> bool {
    value.get("dryRun") == Some(&Value::Bool(true))
}
