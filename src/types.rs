//! Type definitions for CLI schemas, parameters and validation issues (lmts `types.ts`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// A JSON object with insertion-ordered keys (`serde_json` with `preserve_order`).
pub type Record = Map<String, Value>;

/// Transport or element type of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    Json,
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::Number => "number",
            ParamType::Boolean => "boolean",
            ParamType::Object => "object",
            ParamType::Array => "array",
            ParamType::Json => "json",
        }
    }
}

impl fmt::Display for ParamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Element type of an `array` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemType {
    String,
    Number,
    Boolean,
    Object,
}

impl ItemType {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::String => "string",
            ItemType::Number => "number",
            ItemType::Boolean => "boolean",
            ItemType::Object => "object",
        }
    }

    pub fn as_param_type(self) -> ParamType {
        match self {
            ItemType::String => ParamType::String,
            ItemType::Number => ParamType::Number,
            ItemType::Boolean => ParamType::Boolean,
            ItemType::Object => ParamType::Object,
        }
    }
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Cardinality: `1` exactly one (default), `?` zero or one, `+` one or more, `*` zero or more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Occurs {
    #[serde(rename = "1")]
    One,
    #[serde(rename = "?")]
    Optional,
    #[serde(rename = "+")]
    OneOrMore,
    #[serde(rename = "*")]
    ZeroOrMore,
}

impl Occurs {
    pub fn as_str(self) -> &'static str {
        match self {
            Occurs::One => "1",
            Occurs::Optional => "?",
            Occurs::OneOrMore => "+",
            Occurs::ZeroOrMore => "*",
        }
    }
}

/// One declared parameter of an operation (or a global flag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamSpec {
    /// Camel-case param name (as exposed on the CLI).
    pub name: String,
    /// Transport or element type.
    #[serde(rename = "type")]
    pub kind: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurs: Option<Occurs>,
    /// Default value applied during validation if omitted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub default: Option<Value>,
    /// Allowed string or numeric values.
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positional: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<ItemType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<bool>,
}

/// `"default": null` is a present default whose value is null (TS keeps it as `null`, not `undefined`).
fn deserialize_present<'de, D>(d: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(d).map(Some)
}

impl ParamSpec {
    /// A parameter with only a name and a type (occurs defaults to `1`).
    pub fn new(name: impl Into<String>, kind: ParamType) -> Self {
        ParamSpec {
            name: name.into(),
            kind,
            occurs: None,
            default: None,
            enum_values: None,
            positional: None,
            items: None,
            description: None,
            aliases: None,
            cli_only: None,
            doc_only: None,
            derived: None,
        }
    }

    pub fn occurs(mut self, occurs: Occurs) -> Self {
        self.occurs = Some(occurs);
        self
    }

    pub fn optional(self) -> Self {
        self.occurs(Occurs::Optional)
    }

    pub fn with_default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    pub fn with_enum<I: IntoIterator<Item = Value>>(mut self, values: I) -> Self {
        self.enum_values = Some(values.into_iter().collect());
        self
    }

    pub fn positional(mut self) -> Self {
        self.positional = Some(true);
        self
    }

    pub fn items(mut self, items: ItemType) -> Self {
        self.items = Some(items);
        self
    }

    pub fn describe(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.aliases = Some(aliases.into_iter().map(Into::into).collect());
        self
    }

    pub(crate) fn is_positional(&self) -> bool {
        self.positional == Some(true)
    }

    pub(crate) fn enum_list(&self) -> Option<&[Value]> {
        match &self.enum_values {
            Some(v) if !v.is_empty() => Some(v.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn alias_list(&self) -> &[String] {
        self.aliases.as_deref().unwrap_or(&[])
    }
}

/// How an operation affects state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
    Dynamic,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Dynamic => "dynamic",
        }
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Declared result shape of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReturnType {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
}

/// The full input shape of one operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<ReturnType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub params: Vec<ParamSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<bool>,
}

impl OperationSchema {
    pub fn new(name: impl Into<String>) -> Self {
        OperationSchema {
            name: name.into(),
            access: None,
            returns: None,
            description: None,
            params: Vec::new(),
            additional_params: None,
        }
    }

    pub fn access(mut self, access: Access) -> Self {
        self.access = Some(access);
        self
    }

    pub fn describe(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn param(mut self, p: ParamSpec) -> Self {
        self.params.push(p);
        self
    }

    pub fn additional_params(mut self, on: bool) -> Self {
        self.additional_params = Some(on);
        self
    }

    pub(crate) fn allows_additional(&self) -> bool {
        self.additional_params == Some(true)
    }
}

/// Input forms a tool understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputForm {
    Flags,
    Json,
    Graphql,
}

impl InputForm {
    pub fn as_str(self) -> &'static str {
        match self {
            InputForm::Flags => "flags",
            InputForm::Json => "json",
            InputForm::Graphql => "graphql",
        }
    }
}

/// Policy flags a tool declares in its published document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyFlags {
    pub dryrun: bool,
    pub readonly: bool,
}

/// How a wrapper detects the version of the product it wraps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductDetect {
    pub argv: Vec<String>,
    pub pattern: String,
}

/// The underlying product and its supported version range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductInfo {
    pub name: String,
    pub supported: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detect: Option<ProductDetect>,
}

/// A published `cli.schema.v1` document (`<tool> --schema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedCliSchema {
    /// Always `cli.schema.v1`.
    pub schema: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyFlags>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Vec<InputForm>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globals: Option<Vec<ParamSpec>>,
    pub operations: Vec<OperationSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<ProductInfo>,
}

/// Names no tool, operation, parameter, alias or global may use.
pub const RESERVED_NAMES: [&str; 3] = ["__proto__", "constructor", "prototype"];

pub fn is_reserved_name(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
}

/// Machine-readable code of a [`ValidationIssue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueCode {
    UnknownParam,
    MissingRequired,
    TypeMismatch,
    EnumViolation,
    FormNotAllowed,
    MissingValue,
    ExtraPositional,
    DocumentParse,
    OpMismatch,
    MixedInput,
    DuplicateFlag,
    ReservedName,
    ShellSyntax,
    UnknownTool,
    UnknownOperation,
    FlagBeforeOp,
}

impl IssueCode {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueCode::UnknownParam => "UNKNOWN_PARAM",
            IssueCode::MissingRequired => "MISSING_REQUIRED",
            IssueCode::TypeMismatch => "TYPE_MISMATCH",
            IssueCode::EnumViolation => "ENUM_VIOLATION",
            IssueCode::FormNotAllowed => "FORM_NOT_ALLOWED",
            IssueCode::MissingValue => "MISSING_VALUE",
            IssueCode::ExtraPositional => "EXTRA_POSITIONAL",
            IssueCode::DocumentParse => "DOCUMENT_PARSE",
            IssueCode::OpMismatch => "OP_MISMATCH",
            IssueCode::MixedInput => "MIXED_INPUT",
            IssueCode::DuplicateFlag => "DUPLICATE_FLAG",
            IssueCode::ReservedName => "RESERVED_NAME",
            IssueCode::ShellSyntax => "SHELL_SYNTAX",
            IssueCode::UnknownTool => "UNKNOWN_TOOL",
            IssueCode::UnknownOperation => "UNKNOWN_OPERATION",
            IssueCode::FlagBeforeOp => "FLAG_BEFORE_OP",
        }
    }
}

impl fmt::Display for IssueCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem found while parsing or validating input. Every issue carries a
/// non-empty `message`, `synopsis` and `usage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub code: IssueCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    pub synopsis: String,
    pub usage: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub received: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl ValidationIssue {
    /// An issue with only the required fields set.
    pub fn new(
        code: IssueCode,
        message: impl Into<String>,
        synopsis: impl Into<String>,
        usage: impl Into<String>,
    ) -> Self {
        ValidationIssue {
            code,
            message: message.into(),
            op: None,
            param: None,
            synopsis: synopsis.into(),
            usage: usage.into(),
            received: None,
            allowed: None,
            suggestion: None,
            argv_index: None,
            offset: None,
        }
    }
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Outcome of every parser and validator: a value, or every issue found.
pub type Outcome<T> = Result<T, Vec<ValidationIssue>>;

/// Which input form produced a record (used for `cliOnly` / `docOnly` checks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ValidationOptions {
    pub form: Option<InputForm>,
}

impl ValidationOptions {
    pub fn form(form: InputForm) -> Self {
        ValidationOptions { form: Some(form) }
    }
}

/// Effect classification of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Read,
    Effectful,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Read => "read",
            Effect::Effectful => "effectful",
        }
    }
}

/// A command line interpreted against published schemas.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Interpreted {
    pub tool: String,
    pub op: String,
    pub argv: Vec<String>,
    pub params: Record,
    pub positionals: Vec<String>,
    pub globals: Record,
    pub effect: Effect,
    pub access: Option<Access>,
}
