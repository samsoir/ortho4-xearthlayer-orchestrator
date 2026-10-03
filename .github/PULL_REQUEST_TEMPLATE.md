## Summary

<!-- Brief description of what this PR does and why. -->

## Changes

<!-- Bulleted list of key changes. -->

-

## Related Issues

<!-- Link to related issues: Fixes #N, Related: #N -->

## Test Plan

<!-- How can reviewers verify this change works? Include both automated and manual testing steps. -->

- [ ] `make pre-commit` passes (fmt + clippy + tests)
- [ ] `make verify-db` passes, if the change touches the `TaskStore` port, `oxo-tasks-postgres` or its conformance suite
-

## Checklist

- [ ] A failing test came first (TDD), and acceptance scenarios were added or updated for new behaviour
- [ ] Code follows the project's SOLID principles and patterns
- [ ] Documentation updated if applicable (CLAUDE.md, README, docs/specs/)
- [ ] No new warnings from `cargo clippy`
