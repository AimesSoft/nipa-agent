# Changelog

All notable changes to this project are documented in this file. The project
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-07-27

### Added

- Bounded OpenAI-compatible tool-calling task loop with `submit_result` termination.
- Reusable multi-turn conversation loop for NipaServer's steward.
- Provider compatibility flags, retry classification and `Retry-After` handling.
- Pseudo tool-call recovery and malformed-argument self-correction.
- Token, round, cancellation and tool-output-size guardrails.
- Structured event envelopes for SSE forwarding and transcript persistence.
- Wiremock-backed tests for provider quirks, retries, cancellation and event invariants.

[Unreleased]: https://github.com/AimesSoft/nipa-agent/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/AimesSoft/nipa-agent/releases/tag/v0.1.0
