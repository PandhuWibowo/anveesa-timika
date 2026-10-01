// Strong random passwords that satisfy both sign-in profiles (us-nist: ≥15
// chars · cn-mlps: ≥8 chars, 3 of 4 character classes). Uses the browser's CSPRNG.

const LOWER = 'abcdefghijkmnopqrstuvwxyz' // no l
const UPPER = 'ABCDEFGHJKLMNPQRSTUVWXYZ' // no I, O
const DIGIT = '23456789' // no 0, 1
const SYMBOL = '!@#$%^&*-_=+?'
const ALL = LOWER + UPPER + DIGIT + SYMBOL

function pick(set: string): string {
  // Rejection sampling: no modulo bias.
  const max = 256 - (256 % set.length)
  const b = new Uint8Array(1)
  do crypto.getRandomValues(b)
  while (b[0] >= max)
  return set[b[0] % set.length]
}

export function generatePassword(length = 20, avoid = ''): string {
  for (;;) {
    const chars = [pick(LOWER), pick(UPPER), pick(DIGIT), pick(SYMBOL)]
    while (chars.length < length) chars.push(pick(ALL))
    // Fisher–Yates so the guaranteed classes aren't always first.
    for (let i = chars.length - 1; i > 0; i--) {
      const j = Math.floor((crypto.getRandomValues(new Uint32Array(1))[0] / 2 ** 32) * (i + 1))
      ;[chars[i], chars[j]] = [chars[j], chars[i]]
    }
    const pw = chars.join('')
    if (!avoid || !pw.toLowerCase().includes(avoid.toLowerCase())) return pw
  }
}

export type PolicyRules = { minLength: number; maxLength: number; requireClasses: number }

/** The checks the server will apply (minus history and breach lookups). */
export function passwordProblems(pw: string, username: string, p: PolicyRules): string[] {
  const out: string[] = []
  const len = [...pw].length
  if (len < p.minLength) out.push(`at least ${p.minLength} characters`)
  if (p.maxLength && len > p.maxLength) out.push(`at most ${p.maxLength} characters`)
  const classes = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^A-Za-z0-9]/].filter((r) => r.test(pw)).length
  if (p.requireClasses && classes < p.requireClasses) out.push(`${p.requireClasses} of: lowercase, uppercase, digit, symbol`)
  if (username && pw.toLowerCase().includes(username.toLowerCase())) out.push('must not contain the username')
  return out
}
