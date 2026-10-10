//! Module spec parsing (lmts `modules/spec.ts`): `@owner/repo[#latest|#dev]`.

use super::types::{Channel, ModuleError, ModuleErrorCode, ModuleSpec};

/// `[A-Za-z0-9][A-Za-z0-9._-]*`
pub(crate) fn is_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A valid owner or repo name: `[A-Za-z0-9][A-Za-z0-9._-]*`, not `.` or `..`.
pub(crate) fn is_valid_segment(s: &str) -> bool {
    is_name(s) && s != "." && s != ".."
}

/// `@owner/repo` -> `(owner, repo)`.
fn split_base(base: &str) -> Option<(&str, &str)> {
    let rest = base.strip_prefix('@')?;
    let (owner, repo) = rest.split_once('/')?;
    (is_name(owner) && is_name(repo)).then_some((owner, repo))
}

/// Parse `@owner/repo[#(latest|dev)]` (default channel `latest`). Any other channel or ref
/// is `SPEC_CHANNEL`; syntax and name errors are `SPEC_SYNTAX`.
///
/// ```
/// use lmrust::modules::{parse_spec, Channel};
/// let s = parse_spec("@lmctl-ai/lmgit").unwrap();
/// assert_eq!((s.channel, s.key.as_str()), (Channel::Latest, "@lmctl-ai/lmgit#latest"));
/// assert_eq!(parse_spec("@lmctl-ai/lmgit#v1.0.0").unwrap_err().code.as_str(), "SPEC_CHANNEL");
/// ```
pub fn parse_spec(spec: &str) -> Result<ModuleSpec, ModuleError> {
    if crate::js::trim(spec).is_empty() {
        return Err(ModuleError::new(
            ModuleErrorCode::SpecSyntax,
            "Module spec must be a non-empty string",
        ));
    }
    let (base, channel_part) = match spec.find('#') {
        Some(i) => (&spec[..i], Some(&spec[i + 1..])),
        None => (spec, None),
    };
    let Some((owner, repo)) = split_base(base) else {
        return Err(ModuleError::new(
            ModuleErrorCode::SpecSyntax,
            format!("Invalid module spec syntax: \"{spec}\" (expected @owner/repo[#(latest|dev)])"),
        ));
    };
    // `.` and `..` cannot match (the first character must be alphanumeric); lmts checks them
    // again explicitly, which is unreachable there too.
    let channel = match channel_part {
        None | Some("latest") => Channel::Latest,
        Some("dev") => Channel::Dev,
        Some(_) => {
            return Err(ModuleError::new(
                ModuleErrorCode::SpecChannel,
                "only the latest and dev channels are supported",
            ))
        }
    };
    Ok(ModuleSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        channel,
        key: format!("@{owner}/{repo}#{}", channel.as_str()),
        requested: spec.to_string(),
    })
}
