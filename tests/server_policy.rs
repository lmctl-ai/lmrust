//! Port of lmts `tests/server-policy-errors.test.ts`: server errors (codes, exit codes,
//! `CliError`, `toCliError`), output (`printResult`, `printError`, `exitCodeFor`) and
//! policy (`checkPolicy`, `isDryRunResult`).

use lmrust::server::{
    check_policy, envelope_value, is_dry_run_result, print_error, print_result, CheckContext, CliError, ErrorCode,
    Policy,
};
use lmrust::Access;
use serde_json::{json, Value};

fn record(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().cloned().expect("object")
}

// describe('server/errors')

#[test]
fn exit_codes_cover_all_error_code_values() {
    let expected = [
        (ErrorCode::InvalidRequest, "INVALID_REQUEST", 2),
        (ErrorCode::ValidationError, "VALIDATION_ERROR", 2),
        (ErrorCode::NotFound, "NOT_FOUND", 4),
        (ErrorCode::Conflict, "CONFLICT", 5),
        (ErrorCode::Forbidden, "FORBIDDEN", 6),
        (ErrorCode::Timeout, "TIMEOUT", 7),
        (ErrorCode::ReadOnlyViolation, "READONLY_VIOLATION", 6),
        (ErrorCode::InternalError, "INTERNAL_ERROR", 1),
        (ErrorCode::ServiceUnavailable, "SERVICE_UNAVAILABLE", 8),
        (ErrorCode::CommandFailed, "COMMAND_FAILED", 1),
        (ErrorCode::CommandNotFound, "COMMAND_NOT_FOUND", 127),
        (ErrorCode::MissingConfirmation, "MISSING_CONFIRMATION", 9),
        (ErrorCode::UnsupportedProductVersion, "UNSUPPORTED_PRODUCT_VERSION", 10),
        (ErrorCode::ParseError, "PARSE_ERROR", 3),
        (ErrorCode::UnknownError, "UNKNOWN_ERROR", 1),
    ];
    assert_eq!(ErrorCode::ALL.len(), expected.len());
    for (code, s, exit) in expected {
        assert!(ErrorCode::ALL.contains(&code));
        assert_eq!(code.as_str(), s);
        assert_eq!(code.to_string(), s);
        assert_eq!(ErrorCode::parse(s), Some(code));
        assert_eq!(code.exit_code(), exit, "{s}");
    }
    assert_eq!(ErrorCode::parse("NOPE"), None);
    // the four asserted explicitly in lmts
    assert_eq!(ErrorCode::InvalidRequest.exit_code(), 2);
    assert_eq!(ErrorCode::ReadOnlyViolation.exit_code(), 6);
    assert_eq!(ErrorCode::MissingConfirmation.exit_code(), 9);
    assert_eq!(ErrorCode::CommandNotFound.exit_code(), 127);
}

#[test]
fn cli_error_properties_and_defaults() {
    let err = CliError::new(ErrorCode::Conflict, "conflict message").with_data(record(json!({ "key": "val" })));
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.exit_code(), 5);
    assert_eq!(err.message, "conflict message");
    assert_eq!(err.data, Some(record(json!({ "key": "val" }))));
    assert_eq!(err.to_string(), "CONFLICT: conflict message");

    // Without data
    let err2 = CliError::new(ErrorCode::InternalError, "");
    assert_eq!(err2.data, None);
    assert_eq!(err2.exit_code(), 1);

    // `with` builds data field by field
    let err3 = CliError::new(ErrorCode::NotFound, "x")
        .with("a", json!(1))
        .with("b", json!("two"));
    assert_eq!(err3.data, Some(record(json!({ "a": 1, "b": "two" }))));
}

#[test]
fn to_cli_error_conversions() {
    // A CliError passes through `?` unchanged.
    let original = CliError::new(ErrorCode::NotFound, "not found");
    let passthrough = (|| -> Result<(), CliError> { Err(original.clone())? })().unwrap_err();
    assert_eq!(passthrough, original);

    // Any other error -> UNKNOWN_ERROR with its message
    let io = std::io::Error::other("some error");
    let converted = CliError::from_error(&io);
    assert_eq!(converted.code, ErrorCode::UnknownError);
    assert_eq!(converted.message, "some error");
    let via_from: CliError = std::io::Error::other("some error").into();
    assert_eq!(via_from, converted);

    // Empty message -> fallback ("Unknown error"; lmrust has no custom fallback argument)
    let empty = CliError::from_error(&std::io::Error::other(""));
    assert_eq!(empty.message, "Unknown error");

    let str_err = CliError::from("string failure");
    assert_eq!(str_err.code, ErrorCode::UnknownError);
    assert_eq!(str_err.message, "string failure");

    let num_err = CliError::from(404.to_string());
    assert_eq!(num_err.message, "404");

    let json_err: CliError = serde_json::from_str::<Value>("{").unwrap_err().into();
    assert_eq!(json_err.code, ErrorCode::UnknownError);
    assert!(!json_err.message.is_empty());
}

// describe('server/output')

#[test]
fn print_result_formats_json_to_stdout() {
    let mut out = Vec::new();
    print_result(&mut out, &json!({ "success": true, "count": 42 }));
    let text = String::from_utf8(out).unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({ "success": true, "count": 42 })
    );
    // JSON.stringify(result, null, 2)
    assert_eq!(text, "{\n  \"success\": true,\n  \"count\": 42\n}\n");
}

#[test]
fn print_error_formats_error_envelope_to_stdout() {
    let mut out = Vec::new();
    let err = CliError::new(ErrorCode::ValidationError, "bad input").with("field", json!("name"));
    print_error(&mut out, &err);
    let parsed: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed["success"], false);
    assert_eq!(parsed["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(parsed["error"]["message"], "bad input");
    assert_eq!(parsed["error"]["data"], json!({ "field": "name" }));
    assert_eq!(envelope_value(&err), parsed);

    // No data -> no `data` key
    let bare = envelope_value(&CliError::new(ErrorCode::NotFound, "gone"));
    assert_eq!(
        bare,
        json!({ "success": false, "error": { "code": "NOT_FOUND", "message": "gone" } })
    );
}

#[test]
fn exit_code_for_maps_errors_correctly() {
    assert_eq!(CliError::new(ErrorCode::ReadOnlyViolation, "").exit_code(), 6);
    assert_eq!(CliError::from_error(&std::io::Error::other("generic")).exit_code(), 1);
}

// describe('server/policy')

fn base_policy() -> Policy {
    Policy {
        dry_run: false,
        read_only: false,
        confirmed: false,
    }
}

fn dry_and_ro() -> Policy {
    Policy {
        dry_run: true,
        read_only: true,
        confirmed: false,
    }
}

#[test]
fn read_and_dynamic_access_always_proceed() {
    assert_eq!(
        check_policy(Access::Read, &dry_and_ro(), &CheckContext::new("status")),
        Ok(None)
    );
    assert_eq!(
        check_policy(Access::Dynamic, &dry_and_ro(), &CheckContext::new("query")),
        Ok(None)
    );
}

#[test]
fn write_access_under_read_only_throws_read_only_violation() {
    let policy = Policy {
        read_only: true,
        ..base_policy()
    };
    let ctx = CheckContext::new("commit").command(["git", "commit"]);
    let err = check_policy(Access::Write, &policy, &ctx).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReadOnlyViolation);
    assert_eq!(err.exit_code(), 6);
    let data = err.data.expect("data");
    assert_eq!(data["operation"], "commit");
    assert_eq!(data["command"], json!(["git", "commit"]));
    assert_eq!(
        err.message,
        "Operation \"commit\" is a write operation and this tool is running in read-only mode. Aborting."
    );
}

#[test]
fn write_access_under_dry_run_returns_synthetic_dry_run_result() {
    let policy = Policy {
        dry_run: true,
        ..base_policy()
    };
    let ctx = CheckContext::new("push")
        .command(["git", "push", "origin"])
        .data(record(json!({ "branch": "main" })));
    let res = check_policy(Access::Write, &policy, &ctx)
        .unwrap()
        .expect("dry-run result");
    let v = Value::Object(res);
    assert!(is_dry_run_result(&v));
    assert_eq!(v["success"], true);
    assert_eq!(v["dryRun"], true);
    assert_eq!(v["simulated"], "push");
    assert_eq!(v["command"], json!(["git", "push", "origin"]));
    assert_eq!(v["branch"], "main");
}

#[test]
fn write_access_with_dangerous_requires_confirmation() {
    let ctx = CheckContext::new("drop_database").dangerous(true);
    let err = check_policy(Access::Write, &base_policy(), &ctx).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingConfirmation);
    assert_eq!(err.exit_code(), 9);

    // Confirmed via ctx
    let confirmed_ctx = CheckContext {
        confirmed: true,
        ..ctx.clone()
    };
    assert_eq!(check_policy(Access::Write, &base_policy(), &confirmed_ctx), Ok(None));

    // Confirmed via policy
    let confirmed_policy = Policy {
        confirmed: true,
        ..base_policy()
    };
    assert_eq!(check_policy(Access::Write, &confirmed_policy, &ctx), Ok(None));
}

#[test]
fn regular_write_without_dry_run_or_read_only_proceeds() {
    assert_eq!(
        check_policy(Access::Write, &base_policy(), &CheckContext::new("commit")),
        Ok(None)
    );
}

#[test]
fn is_dry_run_result_helper() {
    assert!(!is_dry_run_result(&Value::Null));
    assert!(!is_dry_run_result(&json!("string")));
    assert!(!is_dry_run_result(&json!({})));
    assert!(!is_dry_run_result(&json!({ "dryRun": false })));
    assert!(is_dry_run_result(&json!({ "dryRun": true, "success": true })));
}
