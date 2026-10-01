// Sign-in page strings: English and Simplified Chinese. Picked from the
// browser language, switchable on the page, remembered per browser.

export type Lang = 'en' | 'zh'

function initial(): Lang {
  try {
    const saved = localStorage.getItem('timika-lang')
    if (saved === 'en' || saved === 'zh') return saved
  } catch { /* private mode */ }
  return navigator.language.toLowerCase().startsWith('zh') ? 'zh' : 'en'
}

export const i18n = $state({ lang: initial() })

export function setLang(l: Lang) {
  i18n.lang = l
  try { localStorage.setItem('timika-lang', l) } catch { /* private mode */ }
}

const zh: Record<string, string> = {
  'Sign in': '登录',
  Account: '账号',
  'Root token': '根令牌',
  Username: '用户名',
  Password: '密码',
  'Show password': '显示密码',
  'Hide password': '隐藏密码',
  "I'm not a robot": '我不是机器人',
  Verifying: '验证中…',
  Verified: '已验证',
  'Loading verification…': '正在加载验证…',
  'proof-of-work': '工作量证明',
  retry: '重试',
  'System use notification': '系统使用须知',
  'I understand and agree': '我已阅读并同意',
  'Privacy notice': '隐私政策',
  'failed attempts lock the account for': '次登录失败将锁定账号',
  min: '分钟',
  'Sessions end after': '会话在无操作',
  'min of inactivity.': '分钟后自动退出。',
  Token: '令牌',
  'Break-glass access with the root token from initialization. Prefer a named account — it\'s what the audit log can attribute.':
    '使用初始化时生成的根令牌进行紧急访问。建议使用实名账号登录，以便审计日志准确记录操作人。',
  'Your password has expired': '密码已过期',
  'Set a new password': '设置新密码',
  'Passwords must be changed every': '密码须每',
  days: '天更换一次。',
  "Your administrator set this password — choose your own before continuing.": '该密码由管理员设置，请先设置您自己的密码。',
  'Current password': '当前密码',
  'New password': '新密码',
  'Repeat new password': '确认新密码',
  'Generate': '生成',
  'Two-factor authentication': '双因素认证',
  'Set up two-factor authentication': '设置双因素认证',
  'Your organization requires a second step when signing in. It takes a minute.': '你的组织要求登录时进行第二步验证，只需一分钟即可完成设置。',
  'Enter the 6-digit code from your authenticator app.': '请输入身份验证器应用中的 6 位验证码。',
  'Enter one of your recovery codes.': '请输入一个恢复码。',
  'Lost your phone? Use a recovery code': '手机丢失？使用恢复码',
  'Use the authenticator app': '使用身份验证器应用',
  'Verify': '验证',
  'Back': '返回',
  'Signed in with a recovery code': '已使用恢复码登录',
  'left': '个剩余',
  'Generate a strong password': '生成强密码',
  'Copy': '复制',
  'Copied': '已复制',
  'Save it in your password manager now — it is not shown again.': '请立即保存到密码管理器——之后不会再显示。',
  'Change password': '修改密码',
  Cancel: '取消',
  'At least': '至少',
  characters: '个字符',
  'of: lowercase, uppercase, digit, symbol': '类：小写字母、大写字母、数字、符号',
  'Does not contain your username': '不包含用户名',
  'Both entries match': '两次输入一致',
  'Not one of your last': '不能与最近',
  passwords: '次使用过的密码相同',
  'Not a commonly used password': '不是常见弱密码',
  'Password changed. Sign in with your new password.': '密码已修改，请使用新密码登录。',
}

/** Translate a UI string (falls back to English). */
export function t(s: string): string {
  return i18n.lang === 'zh' ? (zh[s] ?? s) : s
}
