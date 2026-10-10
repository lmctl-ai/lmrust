//! Server runtime (lmts `./server`): the CLI runner, policy, JSON envelopes and exit
//! codes, process execution and the product version check.

pub mod cli;
pub mod errors;
pub mod exec;
pub mod output;
pub mod policy;
pub mod product;

pub use cli::{
    dry_run_from_env, global_flags, read_only_from_env, run_cli, run_cli_io, schema_document, synthesize_schema,
    CliSpec, ContextBuilder, Handler, OpDef, ProductChecker,
};
pub use errors::{CliError, ErrorCode};
pub use exec::{spawn_command, ExecError, ExecOptions, ExecResult};
pub use output::{envelope_value, print_error, print_result, ErrorBody, ErrorEnvelope};
pub use policy::{check_policy, is_dry_run_result, CheckContext, Policy};
pub use product::{
    check_product_version, find_executable_on_path, reset_product_version_cache, CheckProductVersionOptions,
    DetectExec, ProductCheckFailure, ProductCheckResult,
};
