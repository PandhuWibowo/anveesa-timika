// Captcha client side: the built-in proof-of-work solver and loaders for the
// third-party widgets. What the server expects back is `{ provider, token }`.

export type CaptchaConfig = {
  provider: 'none' | 'pow' | 'turnstile' | 'recaptcha' | 'hcaptcha' | 'geetest' | 'tencent'
  fallback: 'pow' | null
  site_key?: string
  /** recaptcha: www.google.com, or www.recaptcha.net (reachable from mainland China) */
  domain?: string
}

export type CaptchaValue = { provider: string; token: string }

// ─── proof-of-work ──────────────────────────────────────────────────────────
// Runs in a Web Worker with a self-contained SHA-256, so it works without
// crypto.subtle (plain-HTTP intranet deployments) and never blocks the page.

const WORKER_SRC = `
const K = new Uint32Array([0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,
0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,
0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,
0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,
0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]);
function sha256hex(str) {
  const bytes = new TextEncoder().encode(str);
  const l = bytes.length, n = ((l + 9 + 63) >> 6) << 6;
  const m = new Uint8Array(n); m.set(bytes); m[l] = 0x80;
  const dv = new DataView(m.buffer); dv.setUint32(n - 4, l * 8); dv.setUint32(n - 8, Math.floor(l / 0x20000000));
  const H = new Uint32Array([0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19]);
  const W = new Uint32Array(64);
  for (let o = 0; o < n; o += 64) {
    for (let i = 0; i < 16; i++) W[i] = dv.getUint32(o + i * 4);
    for (let i = 16; i < 64; i++) {
      const a = W[i-15], b = W[i-2];
      const s0 = ((a>>>7)|(a<<25)) ^ ((a>>>18)|(a<<14)) ^ (a>>>3);
      const s1 = ((b>>>17)|(b<<15)) ^ ((b>>>19)|(b<<13)) ^ (b>>>10);
      W[i] = (W[i-16] + s0 + W[i-7] + s1) | 0;
    }
    let [a,b,c,d,e,f,g,h] = H;
    for (let i = 0; i < 64; i++) {
      const S1 = ((e>>>6)|(e<<26)) ^ ((e>>>11)|(e<<21)) ^ ((e>>>25)|(e<<7));
      const t1 = (h + S1 + ((e & f) ^ (~e & g)) + K[i] + W[i]) | 0;
      const S0 = ((a>>>2)|(a<<30)) ^ ((a>>>13)|(a<<19)) ^ ((a>>>22)|(a<<10));
      const t2 = (S0 + ((a & b) ^ (a & c) ^ (b & c))) | 0;
      h = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
    }
    H[0]+=a; H[1]+=b; H[2]+=c; H[3]+=d; H[4]+=e; H[5]+=f; H[6]+=g; H[7]+=h;
  }
  return Array.from(H, x => (x >>> 0).toString(16).padStart(8, '0')).join('');
}
onmessage = (ev) => {
  const { salt, challenge, maxnumber } = ev.data;
  for (let n = 0; n <= maxnumber; n++) {
    if (sha256hex(salt + n) === challenge) { postMessage({ number: n }); return; }
  }
  postMessage({ number: null });
};`

type Challenge = { algorithm: string; challenge: string; maxnumber: number; salt: string; signature: string }

export async function solvePow(fetchChallenge: () => Promise<Challenge>): Promise<string> {
  const c = await fetchChallenge()
  const url = URL.createObjectURL(new Blob([WORKER_SRC], { type: 'text/javascript' }))
  try {
    const number = await new Promise<number | null>((resolve, reject) => {
      const w = new Worker(url)
      w.onmessage = (e) => { resolve(e.data.number); w.terminate() }
      w.onerror = (e) => { reject(e); w.terminate() }
      w.postMessage(c)
    })
    if (number === null) throw new Error('could not solve the challenge')
    const payload = { algorithm: c.algorithm, challenge: c.challenge, number, salt: c.salt, signature: c.signature }
    return btoa(JSON.stringify(payload))
  } finally {
    URL.revokeObjectURL(url)
  }
}

// ─── third-party widgets ────────────────────────────────────────────────────

const loaded = new Map<string, Promise<void>>()

/** Load a script once; rejects on error or after `timeoutMs` (blocked domains hang). */
export function loadScript(src: string, timeoutMs = 8000): Promise<void> {
  if (!loaded.has(src)) {
    loaded.set(src, new Promise<void>((resolve, reject) => {
      const s = document.createElement('script')
      s.src = src
      s.async = true
      const timer = setTimeout(() => reject(new Error(`timed out loading ${new URL(src).host}`)), timeoutMs)
      s.onload = () => { clearTimeout(timer); resolve() }
      s.onerror = () => { clearTimeout(timer); reject(new Error(`could not load ${new URL(src).host}`)) }
      document.head.appendChild(s)
    }).catch((e) => { loaded.delete(src); throw e }))
  }
  return loaded.get(src)!
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const w = window as any

export const SCRIPTS: Record<string, (cfg: CaptchaConfig) => string> = {
  turnstile: () => 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit',
  recaptcha: (c) => `https://${c.domain || 'www.google.com'}/recaptcha/api.js?render=explicit`,
  hcaptcha: () => 'https://js.hcaptcha.com/1/api.js?render=explicit',
  geetest: () => 'https://static.geetest.com/v4/gt4.js',
  tencent: () => 'https://turing.captcha.qcloud.com/TJCaptcha.js',
}

/** Wait for a global a script defines (some register it a tick after onload). */
async function global<T>(name: string): Promise<T> {
  for (let i = 0; i < 50; i++) {
    if (w[name]) return w[name] as T
    await new Promise((r) => setTimeout(r, 100))
  }
  throw new Error(`${name} did not initialize`)
}

/**
 * Render an in-page widget (turnstile / recaptcha / hcaptcha) into `el`.
 * `onToken` gets the token (or '' when it expires). Returns a reset function.
 */
export async function renderWidget(cfg: CaptchaConfig, el: HTMLElement, theme: 'dark' | 'light', onToken: (t: string) => void): Promise<() => void> {
  await loadScript(SCRIPTS[cfg.provider](cfg))
  const opts = {
    sitekey: cfg.site_key,
    theme,
    callback: (t: string) => onToken(t),
    'expired-callback': () => onToken(''),
    'error-callback': () => onToken(''),
  }
  if (cfg.provider === 'turnstile') {
    const ts = await global<{ render: (e: HTMLElement, o: object) => string; reset: (id: string) => void }>('turnstile')
    const id = ts.render(el, opts)
    return () => ts.reset(id)
  }
  const name = cfg.provider === 'recaptcha' ? 'grecaptcha' : 'hcaptcha'
  const api = await global<{ render: (e: HTMLElement, o: object) => number; reset: (id: number) => void; ready?: (f: () => void) => void }>(name)
  if (api.ready) await new Promise<void>((r) => api.ready!(r))
  const id = api.render(el, opts)
  return () => api.reset(id)
}

/** Pop-up style challenges (geetest v4 / tencent): resolve with the token JSON. */
export async function runPopup(cfg: CaptchaConfig): Promise<string> {
  await loadScript(SCRIPTS[cfg.provider](cfg))
  if (cfg.provider === 'geetest') {
    const init = await global<(o: object, cb: (c: unknown) => void) => void>('initGeetest4')
    return new Promise((resolve, reject) => {
      init({ captchaId: cfg.site_key, product: 'bind', language: navigator.language.startsWith('zh') ? 'zho' : 'eng' }, (c: any) => {
        c.onReady(() => c.showCaptcha())
          .onSuccess(() => resolve(JSON.stringify(c.getValidate())))
          .onError((e: unknown) => reject(new Error(`GeeTest: ${JSON.stringify(e)}`)))
          .onClose(() => reject(new Error('captcha closed')))
      })
    })
  }
  const TC = await global<new (appId: string, cb: (r: any) => void, o?: object) => { show: () => void }>('TencentCaptcha')
  return new Promise((resolve, reject) => {
    new TC(cfg.site_key!, (r) => {
      if (r.ret === 0) resolve(JSON.stringify({ ticket: r.ticket, randstr: r.randstr }))
      else reject(new Error('captcha closed'))
    }, { userLanguage: navigator.language.startsWith('zh') ? 'zh-cn' : 'en' }).show()
  })
}
