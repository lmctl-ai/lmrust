# Policy — `server/policy.rs`

| Access | --dryrun | --readonly | --confirm |
|---|---|---|---|
| read | runs | runs | n/a |
| dynamic | runs (op decides at run time) | runs | n/a |
| write | synthetic `{success, dryRun, simulated, ...data, command?}`, nothing executed | `READONLY_VIOLATION`, exit 6 | needed only when the call site marks it `dangerous` (`MISSING_CONFIRMATION`, exit 9) |

- Policy per invocation: `--dryrun`/`--dry-run`, `--readonly`/`--read-only`,
  `--confirm`/`--i-understand`, plus env floors `<TOOL>_DRYRUN` and `<TOOL>_READONLY`
  (`1|true|yes|on`, case-insensitive; tool name upper-cased, non-alphanumerics -> `_`).
  Flags can only add to the floor, never loosen it.
- `check_policy(access, &policy, &CheckContext)` is called by the handler (the runner does not
  know which calls are dangerous): `Ok(None)` proceed, `Ok(Some(result))` return the dry-run
  result, `Err` read-only or missing confirmation. Read-only wins over dry-run.
- Approval/confirmation UX beyond these inert defaults is deferred by decision.
