//! Unified input resolution (lmts `inputs.ts`): one of `--input-file` / `--input-json`
//! / `--input-graphql` (a document) or the command-line flags. Documents and op flags
//! cannot be mixed on one invocation; runner globals may accompany a document.

use crate::argv_form::parse_argv_form;
use crate::graphql_form::{parse_graphql_input, validate_graphql_params};
use crate::json_form::{parse_json_input, validate_json_params};
use crate::types::{InputForm, OperationSchema, ParamSpec, Record, ValidationIssue, ValidationOptions};
use crate::validate::{validate, SchemaValidationError};
use serde_json::Value;

/// The params of one invocation and the form that produced them.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedInput {
    pub kind: InputForm,
    /// Typed params, defaults applied, enums checked.
    pub params: Record,
}

/// Raw inputs of one invocation.
#[derive(Debug, Clone, Copy)]
pub struct ResolveArgs<'a> {
    pub schema: &'a OperationSchema,
    /// CLI tokens after the op name (already split by the host shell).
    pub argv: &'a [String],
    /// Runner-owned global flags; extracted from argv, never reported unknown.
    pub globals: &'a [ParamSpec],
    /// JSON document (from `--input-file` or `--input-json`).
    pub json_document: Option<&'a str>,
    /// GraphQL document (from `--input-graphql`).
    pub graphql_document: Option<&'a str>,
}

impl<'a> ResolveArgs<'a> {
    /// Flags-only resolution of `argv` against `schema`.
    pub fn new(schema: &'a OperationSchema, argv: &'a [String]) -> Self {
        ResolveArgs {
            schema,
            argv,
            globals: &[],
            json_document: None,
            graphql_document: None,
        }
    }
}

fn throw_issues(schema: &OperationSchema, issues: &[ValidationIssue]) -> SchemaValidationError {
    SchemaValidationError::from_issues(&schema.name, issues, Record::new())
}

fn mismatch(schema: &OperationSchema, message: String, received: &str) -> SchemaValidationError {
    let mut details = Record::new();
    details.insert("expected".into(), Value::String(schema.name.clone()));
    details.insert("received".into(), Value::String(received.to_string()));
    SchemaValidationError::new(schema.name.clone(), message, details)
}

/// Resolve the effective params of one invocation.
pub fn resolve_input(args: ResolveArgs<'_>) -> Result<ResolvedInput, SchemaValidationError> {
    let schema = args.schema;
    let has_json = args.json_document.is_some();
    let has_graphql = args.graphql_document.is_some();

    if has_json && has_graphql {
        return Err(SchemaValidationError::new(
            schema.name.clone(),
            "cannot mix --input-json and --input-graphql",
            Record::new(),
        ));
    }
    if (has_json || has_graphql) && !args.argv.is_empty() {
        // Global flags may accompany a document; op flags and positionals may not.
        let mixed = match parse_argv_form(args.argv, schema, args.globals) {
            Ok(v) => !v.params.is_empty() || !v.positionals.is_empty(),
            Err(_) => true,
        };
        if mixed {
            let which = if has_json { "--input-json" } else { "--input-graphql" };
            return Err(SchemaValidationError::new(
                schema.name.clone(),
                format!("cannot mix {which} with other flags"),
                Record::new(),
            ));
        }
    }

    if let Some(text) = args.json_document {
        let doc = parse_json_input(text)?;
        if let Some(op) = doc.op.as_deref().filter(|op| *op != schema.name) {
            return Err(mismatch(
                schema,
                format!(
                    "input document op \"{}\" does not match invoked op \"{}\"",
                    op, schema.name
                ),
                op,
            ));
        }
        return Ok(ResolvedInput {
            kind: InputForm::Json,
            params: validate_json_params(schema, &doc)?,
        });
    }

    if let Some(text) = args.graphql_document {
        let doc = parse_graphql_input(text)?;
        if let Some(op) = doc.op.as_deref().filter(|op| *op != schema.name) {
            return Err(mismatch(
                schema,
                format!(
                    "graphql selection \"{}\" does not match invoked op \"{}\"",
                    op, schema.name
                ),
                op,
            ));
        }
        return Ok(ResolvedInput {
            kind: InputForm::Graphql,
            params: validate_graphql_params(schema, &doc)?,
        });
    }

    // Command-line form: argv is used as the shell split it, never re-joined.
    let parsed = parse_argv_form(args.argv, schema, args.globals).map_err(|i| throw_issues(schema, &i))?;
    let validated = validate(schema, &parsed.params, ValidationOptions::form(InputForm::Flags))
        .map_err(|i| throw_issues(schema, &i))?;
    Ok(ResolvedInput {
        kind: InputForm::Flags,
        params: validated,
    })
}
