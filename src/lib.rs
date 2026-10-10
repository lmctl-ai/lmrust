//! # lmrust — the Rust SDK for lmapi
//!
//! [lmapi](https://github.com/lmctl-ai/lmapi) is the API for the LLM era: a CLI is the
//! server, `<tool> --schema` publishes a `cli.schema.v1` document as the contract, and an
//! LLM or any client calls the tool through one validated command line. `lmrust` is the
//! Rust implementation of that contract, in parity with the TypeScript SDK
//! [lmts](https://github.com/lmctl-ai/lmts):
//!
//! - **One validator** ([`validate()`]) for three input forms: command-line flags
//!   ([`argv_form`]), a JSON document ([`json_form`]) and a GraphQL document ([`graphql_form`]).
//! - **Publish and check** `cli.schema.v1` documents ([`publish`]), derived help ([`help`]),
//!   client-side encoders ([`encode`]).
//! - **Interpreter** for LLM-produced command lines ([`interpret`]): safe tokenizer,
//!   validation, effect classification and prompt rendering.
//! - **Server runtime** ([`server`]): `run_cli` turns a schema plus one handler per
//!   operation into a complete lmapi tool (`--schema`, `--help`, document inputs, policy
//!   flags, JSON envelopes, exit codes, product version checks).
//! - **Module store** (`modules`, cargo feature `modules`): the `@owner/repo` schema
//!   cache shared with lmts.
//!
//! ```
//! use lmrust::{validate, OperationSchema, ParamSpec, ParamType, ValidationOptions};
//! use serde_json::json;
//!
//! let op = OperationSchema::new("commit")
//!     .param(ParamSpec::new("message", ParamType::String))
//!     .param(ParamSpec::new("amend", ParamType::Boolean).optional());
//! let raw = json!({"message": "fix", "amend": "true"});
//! let params = validate(&op, raw.as_object().unwrap(), ValidationOptions::default()).unwrap();
//! assert_eq!(params["amend"], json!(true));
//! ```

pub mod argv_form;
pub mod encode;
pub mod graphql_form;
pub mod help;
pub mod inputs;
pub mod interpret;
pub mod js;
pub mod json_form;
pub mod publish;
pub mod server;
pub mod synopsis;
pub mod types;
pub mod validate;
pub mod version;

pub use argv_form::{coerce_json_literal, parse_argv_form, ArgvFormValue};
pub use encode::{encode_argv, encode_graphql, encode_json, EncodeArgvOptions, EncodeError};
pub use graphql_form::{parse_graphql_document, parse_graphql_input, validate_graphql_params, GraphqlInputDocument};
pub use help::{render_op_help, render_tool_help};
pub use inputs::{resolve_input, ResolveArgs, ResolvedInput};
pub use interpret::{
    classify_operation, describe_for_prompt, interpret_command, is_effectful, tokenize_simple_command,
};
pub use json_form::{parse_json_document, parse_json_input, validate_json_params, JsonInputDocument};
pub use publish::{
    find_operation, parse_published_schema, parse_published_schema_or_err, publish_schema, schema_warnings,
    PublishOptions,
};
pub use synopsis::{build_synopsis, is_required, synopsis_for_param, to_camel, to_kebab, usage_line};
pub use types::*;
pub use validate::{to_array_value, validate, validate_params, SchemaValidationError};
pub use version::{
    compare_versions, count_regexp_groups, is_valid_supported_range, parse_version, satisfies, ParsedVersion,
};
