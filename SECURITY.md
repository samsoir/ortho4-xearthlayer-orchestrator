# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest release | Yes |
| Older releases | No |

Only the latest release receives security updates. We recommend always running the most recent version.

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

Instead, please report vulnerabilities privately:

1. **GitHub Security Advisories** (preferred): Use [Report a vulnerability](https://github.com/samsoir/ortho4-xearthlayer-orchestrator/security/advisories/new) to submit a private report directly on GitHub.
2. **Email**: Contact the maintainers directly if the advisory feature is unavailable.

### What to Include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if you have one)

### What to Expect

- **Acknowledgment** within 48 hours
- **Assessment** within 1 week
- **Fix timeline** communicated once the severity is assessed
- **Credit** in the release notes (unless you prefer to remain anonymous)

## Scope

OXO runs a control plane daemon (`oxo-controld`) with an HTTP API and a PostgreSQL store, plus worker pods that claim tasks over that API and run Ortho4XP. Areas of particular security concern include:

- **The HTTP API** — it has no built-in authentication in v1 and is meant to be reachable only from a trusted network; input validation of submitted region specifications is in scope
- **Region specification parsing** — path handling in `target.root`, and the refusal of raw Ortho4XP application-level keys
- **Worker file handling** — delivery of results to the artifacts volume (the egress whitelist), and the config overlay installed over the Ortho4XP install root
- **Database access** — the control plane is the only component that holds database credentials; workers never reach the persistence layer
- **Container image** — the worker pod image and its mounts (patches and scenery are read-only)
- **Dependency vulnerabilities** — outdated or compromised crates

## Security Practices

- Workers talk to the control plane over HTTP only; OXO holds no container runtime credentials
- Inputs from specifications and API requests are validated before work is planned
- No secrets are stored in the repository
