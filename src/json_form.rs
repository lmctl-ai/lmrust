//! JSON input form (lmts `json-form.ts`): a bare params object, or the envelope
//! `{ "op": "...", "params": {...} }` (recognised only when `op` is a string).

use crate::js;
use crate::synopsis::usage_line;
use crate::types::{IssueCode, OperationSchema, Outcome, Record, ValidationIssue};
use crate::validate::{validate_params, SchemaValidationError};
use serde_json::Value;

/// A parsed JSON input document.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonInputDocument {
    /// The envelope's op, when the document used `{op, params}`.
    pub op: Option<String>,
    pub params: Record,
}

fn received_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Array(_) => "array",
        other => js::type_of(other),
    }
}

/// Parse a JSON document string into op and params.
pub fn parse_json_document(text: &str) -> Outcome<JsonInputDocument> {
    let fallback_usage = usage_line(&OperationSchema::new("<unknown>"), None);
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return Err(vec![ValidationIssue::new(
                IssueCode::DocumentParse,
                format!("Invalid JSON: {e}. Expected: {fallback_usage}"),
                fallback_usage.clone(),
                fallback_usage,
            )]);
        }
    };
    let Value::Object(obj) = parsed else {
        let mut i = ValidationIssue::new(
            IssueCode::DocumentParse,
            format!(
                "JSON input must be an object, received {}. Expected: {}",
                received_type(&parsed),
                fallback_usage
            ),
            fallback_usage.clone(),
            fallback_usage,
        );
        i.received = Some(parsed);
        return Err(vec![i]);
    };

    if let Some(Value::String(op)) = obj.get("op") {
        let op = op.clone();
        let op_usage = usage_line(&OperationSchema::new(op.clone()), None);
        if let Some(params) = obj.get("params") {
            let Value::Object(params) = params else {
                let mut i = ValidationIssue::new(
                    IssueCode::DocumentParse,
                    format!(
                        "\"params\" in envelope must be an object, received {}. Expected: {}",
                        received_type(params),
                        op_usage
                    ),
                    op_usage.clone(),
                    op_usage,
                );
                i.op = Some(op);
                i.received = Some(params.clone());
                return Err(vec![i]);
            };
            return Ok(JsonInputDocument {
                op: Some(op),
                params: params.clone(),
            });
        }
        return Ok(JsonInputDocument {
            op: Some(op),
            params: Record::new(),
        });
    }

    Ok(JsonInputDocument { op: None, params: obj })
}

/// Throwing-style wrapper around [`parse_json_document`] (lmts `parseJsonInput`).
pub fn parse_json_input(text: &str) -> Result<JsonInputDocument, SchemaValidationError> {
    parse_json_document(text).map_err(|issues| {
        let first = &issues[0];
        let mut details = Record::new();
        details.insert("code".into(), Value::String(first.code.as_str().into()));
        details.insert("synopsis".into(), Value::String(first.synopsis.clone()));
        details.insert("issues".into(), serde_json::to_value(&issues).unwrap_or(Value::Null));
        SchemaValidationError::new(
            first.op.clone().unwrap_or_else(|| "<unknown>".into()),
            first.message.clone(),
            details,
        )
    })
}

/// Validate a document's params against the op schema.
pub fn validate_json_params(
    schema: &OperationSchema,
    doc: &JsonInputDocument,
) -> Result<Record, SchemaValidationError> {
    validate_params(schema, &doc.params)
}
