# Security Policy

Only the latest tagged `0.x` release and the current `main` branch are
supported while the project is in active development.

Report vulnerabilities privately through GitHub's **Security → Report a
vulnerability** flow for `AimesSoft/nipa-agent`. Do not open a public issue
with working payloads, credentials or private model responses.

Relevant reports include API-key exposure, unsafe URL or header handling,
prompt/tool boundary violations, unbounded resource consumption, cancellation
failures and event/transcript leakage. Security problems in an upstream model
provider are out of scope unless the runtime handles its response unsafely.
