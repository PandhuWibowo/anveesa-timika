# Security policy

timika stores secrets and brokers access to servers, so security reports get
priority over everything else.

## Reporting a vulnerability

**Please do not open a public issue.** Report it privately:

1. Go to the repository's **Security** tab → **Report a vulnerability**
   (GitHub private vulnerability reporting).
2. Describe what you found, how to reproduce it, and what an attacker gains.
   A proof of concept helps; a fix is welcome but not required.

You can expect an acknowledgement within 3 days and a first assessment within
7 days. Once a fix is released the report is credited to you, unless you prefer
otherwise.

## Scope

Anything that breaks one of the security invariants in [CLAUDE.md](CLAUDE.md)
is in scope, for example:

- reading or writing vault data without the root key, or plaintext at rest;
- bypassing sign-in, two-factor, lockout, roles or server grants;
- command injection through any value sent to a server (container, network
  tool, archive and automation commands);
- secrets or tokens appearing in logs, the audit trail or API responses;
- joining a Raft cluster or calling cluster RPCs without the root key;
- tampering with the audit log without `audit-verify` noticing.

Out of scope: denial of service by an authenticated administrator, and issues
that need an already compromised host or browser.

## Supported versions

The project is pre-1.0. Fixes land on `main`; there are no maintained release
branches yet.

## Please note

timika has **not had an independent security audit**. Evaluate it before
trusting it with production secrets.
