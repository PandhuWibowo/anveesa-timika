# Sign-in: accounts, policy profiles, captcha

The login page takes **username + password + captcha**, and the system-use
notice when the policy requires it. The **root token** remains as a separate
break-glass tab. Accounts live inside the vault itself: barrier-encrypted at
`auth/userpass/users/<name>`, passwords hashed with Argon2id.

## Policy profiles (`LOGIN_POLICY`)

| | `us-nist` (default) | `cn-mlps` |
|---|---|---|
| Basis | NIST SP 800-63B rev. 4 · SP 800-53 AC-7 / AC-8 | GB/T 22239-2019 等级保护 2.0, level 3 |
| Min / max length | **15** (password is the only factor) / 128 | **8** / 128 |
| Composition rule | none (800-63B forbids them) | **3 of 4**: lower, upper, digit, symbol |
| Common/breached list | yes (+ optional Have I Been Pwned: `PASSWORD_BREACH_CHECK=hibp`) | yes (incl. common Chinese passwords such as `woaini1314`) |
| Contains username | rejected | rejected |
| Expiry | none (800-63B: don't force periodic change) | **90 days**, then change on next sign-in |
| History | – | last **5** can't be reused |
| Failed sign-ins | **10** → locked 15 min (AC-7; 800-63B caps at 100) | **5** → locked **30 min** (登录失败处理) |
| Idle timeout / max session | 30 min / 12 h | 30 min / 8 h (超时自动退出) |
| Notice before sign-in | "authorized use / monitoring" banner, **must acknowledge** (AC-8) | optional (`LOGIN_BANNER`, `LOGIN_BANNER_ZH`) |
| Last sign-in shown | time + IP of last success and last failure | same (required by 等保) |

Overrides: `PASSWORD_MIN_LENGTH`, `PASSWORD_MAX_LENGTH`,
`PASSWORD_REQUIRE_CLASSES`, `PASSWORD_MAX_AGE_DAYS`, `PASSWORD_HISTORY`,
`LOCKOUT_THRESHOLD`, `LOCKOUT_MINUTES`, `SESSION_IDLE_MINUTES`,
`SESSION_MAX_HOURS`, `LOGIN_BANNER`, `LOGIN_BANNER_ZH`,
`LOGIN_BANNER_REQUIRE_ACK`, `PRIVACY_NOTICE_URL` (a PIPL / CCPA privacy
notice link).

Behavior common to both profiles:
- **Initial passwords:** an administrator-set password must be changed at first
  sign-in. That session can do nothing else.
- **Paste and show/hide** are allowed in the password field (800-63B).
- **Timing:** unknown users and wrong passwords take the same time, and give
  the same message.
- **Disabling or deleting a user** ends their live sessions immediately.
- **Sessions** renew while you're active, at most once a minute. A warning
  appears 2 minutes before an idle timeout.
- **Logout** revokes the token server-side.

## Captcha (`CAPTCHA_PROVIDER`)

| Provider | US | Mainland China | Server keys |
|---|---|---|---|
| **`pow`** (default) | ✅ | ✅ | none (self-hosted) |
| `turnstile` | ✅ | ⚠️ unreliable | `CAPTCHA_SITE_KEY`, `CAPTCHA_SECRET` |
| `recaptcha` | ✅ | ✅ with `CAPTCHA_RECAPTCHA_DOMAIN=www.recaptcha.net` | `CAPTCHA_SITE_KEY`, `CAPTCHA_SECRET` |
| `hcaptcha` | ✅ | ⚠️ | `CAPTCHA_SITE_KEY`, `CAPTCHA_SECRET` |
| `geetest` (v4) | ✅ | ✅ | `CAPTCHA_GEETEST_ID`, `CAPTCHA_GEETEST_KEY` |
| `tencent` | – | ✅ | `CAPTCHA_TENCENT_APP_ID`, `CAPTCHA_TENCENT_APP_SECRET`, `TENCENTCLOUD_SECRET_ID`, `TENCENTCLOUD_SECRET_KEY` |
| `none` | | | – |

- **`pow`** is an ALTCHA-compatible proof-of-work. The browser spends about
  half a second of CPU in a Web Worker, and there's no puzzle.
  - **Accessibility:** nothing to see or hear, which suits WCAG and ADA.
  - **Privacy:** it calls no third party. No personal data leaves your servers,
    which avoids PIPL cross-border-transfer questions, and nothing is blocked in
    China.
  - **Security:** challenges are signed with a key derived from the root key (so
    they work across replicas), expire after 3 minutes, and are single-use.
    Replay protection is per instance.
- **`CAPTCHA_FALLBACK=pow`:** if the configured provider's script fails to load
  within 8s (e.g. Turnstile inside China), the page switches to the built-in
  captcha. One deployment can serve users in both regions.
- **Verification:** all verification happens server-side, and every captcha
  token is single-use. The captcha is checked before the password, so it can't
  be used to test passwords.

**Verified against the real services with their official test keys:**
Turnstile (pass and fail keys), reCAPTCHA via `www.google.com` **and**
`www.recaptcha.net`, and hCaptcha (valid and invalid responses).

**Not yet verified:** GeeTest and Tencent. They are implemented to their
documentation, and Tencent's TC3 request signing is unit-tested for shape, but
they need an account to test end to end.

## Rate limiting

- **Per client IP**, per instance: `LOGIN_RATE_LIMIT` attempts (30) per
  `LOGIN_RATE_WINDOW_SECS` (300), then 429.
- **Behind a load balancer:** set `TRUST_X_FORWARDED_FOR=true`. The bundled
  HAProxy configs send it.
- **Raft:** followers pass the client IP on when forwarding to the leader.
- **Account lockout**, stored in the vault, applies across all instances.

## Roles

| Role | Can |
|---|---|
| `admin` | everything, including user administration (API) |
| `read-only` | GET only (read secrets, view status); no user administration |
| root token | everything (break-glass; not attributable to a person) |

Anyone signed in can use lookup, renew, revoke and change-password on their
**own** session.

## Two-factor authentication (MFA)

TOTP — the standard authenticator-app codes (Google / Microsoft Authenticator,
1Password, Authy…; RFC 6238, 30 s, 6 digits) — plus 10 one-time recovery codes.

- **Sign-in:** password (+ captcha) → `Login` answers `mfa_required` with a
  short-lived `mfa_token` (no session yet) → `VerifyMfa(mfa_token, code)` → session.
  The challenge lasts 5 minutes and allows 5 wrong codes; every wrong code also
  counts toward the account lockout. A code is accepted once (replay protection)
  within ±30 s of clock drift.
- **Setup:** My account → *Set up two-factor*: scan the QR code, confirm a code,
  save the recovery codes (shown once; only their SHA-256 is stored). The secret
  lives in the barrier-encrypted user record and never reaches the audit log.
- **Policy:** `MFA_REQUIRED=off|admins|all`. People who must and haven't get a
  setup-only session (like the forced password change) and a full session as
  soon as setup is confirmed. Required 2FA can't be turned off by the user.
- **Lost phone:** sign in with a recovery code, or an administrator uses
  *People → Reset 2FA*. The root token (break-glass) is never subject to 2FA.

## API (gRPC, see [API.md](API.md))

| RPC | Auth |
|---|---|
| `AuthService/GetLoginConfig` | – |
| `AuthService/GetCaptchaChallenge` | – (unsealed) |
| `AuthService/Login` `{username, password, captcha:{provider, token}, banner_ack}` | – |
| `AuthService/RenewSelf` · `RevokeSelf` | session |
| `AuthService/ChangePassword` `{old_password, new_password}` | session |
| `AuthService/ListUsers` | admin |
| `AuthService/GetUser` · `UpsertUser` `{username, password?, policies?, must_change_password?, disabled?}` · `DeleteUser` | admin |
| `AuthService/UnlockUser` | admin |

The audit log records every sign-in attempt with the username on success.
Passwords and tokens never appear in it (see LOGGING.md).

## Languages

The login page, password-change form and captcha control are available in
**English and Simplified Chinese**. The language follows the browser and can be
switched on the page. `LOGIN_BANNER_ZH` sets the Chinese text of the notice.
