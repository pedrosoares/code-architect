---
id: adr.plaintext-credential-storage
type: adr
title: 'ADR: Plaintext Credential Storage'
---

# Plaintext Credential Storage

## Status

Accepted

## Context

The engine needs credentials for three integrations (GitHub, Slack, Linear) and for model providers (LM Studio / OpenAI-compatible / Anthropic API keys). These are stored in `integrations.db` (via `IntegrationsConfig`) and `config.json` (via `ModelConfig`). The question is whether to encrypt them at rest.

The realistic threat model: this is a **local, single-user desktop app** on the user's own machine. The data store already contains the user's full coding sessions, file diffs, and the docs vault — far more sensitive than an API key, all of it already unencrypted. Adding encryption for credentials only, while everything else sits in plaintext, gives a false sense of security and doesn't protect against the actual threats (physical access, a full-disk read, a malware process running as the user).

The alternatives were rejected:
- **OS keychain (keyring crate):** adds a native dependency and a per-OS failure mode, prompts on first write, and still only protects against the same physical-access threat as the rest of the data — while making the app's data portable across machines impossible (a keychain item can't travel with the config).
- **A file-level key:** the key has to live somewhere the app can read; that "somewhere" is on the same disk as the ciphertext, so it protects only against a *partial* read of the file, not a full-disk read. It adds complexity for a marginal, non-real threat.

## Decision

Store credentials **in plaintext** in `integrations.db` and `config.json`. Document it plainly; do not pretend the files are secure.

## Consequences

**Positive**
- Simple, portable config: copy the files, the app works on the new machine.
- No native keychain dependency, no prompt-on-first-write, no per-OS failure modes.
- Honest: the whole `.coder`/data directory is already plaintext, so the credentials aren't the weakest link.

**Negative / accepted costs**
- Anyone with file read access to the data directory gets the API tokens. Mitigated by the same measure that protects everything else: the user's OS file permissions on the data directory.
- If the data directory is ever synced or shared, the credentials travel with it — the user must not do that.

**Enforcement**
- Captured as a rule, [Plaintext credentials](../../03-business-rules/plaintext-credentials.md).
- Do **not** add a keychain/encryption layer without revisiting this ADR — the decision is explicit, not an oversight.