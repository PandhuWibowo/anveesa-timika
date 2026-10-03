// Nginx page helpers: new-site templates and small formatters.
import type { NginxApply, NginxSite } from '../gen/timika/v1/nginx_pb'

export type Kind = 'proxy' | 'static' | 'redirect' | 'empty'
export const KINDS: { id: Kind; label: string; field: string; placeholder: string }[] = [
  { id: 'proxy', label: 'Reverse proxy', field: 'Forward to', placeholder: 'http://127.0.0.1:3000' },
  { id: 'static', label: 'Static files', field: 'Folder', placeholder: '/var/www/example' },
  { id: 'redirect', label: 'Redirect', field: 'Redirect to', placeholder: 'https://www.example.com' },
  { id: 'empty', label: 'Empty', field: '', placeholder: '' },
]

/** A starting point for a new site; plain HTTP, so it works before there is a certificate. */
export function template(kind: Kind, domain: string, target: string): string {
  const name = domain.trim() || 'example.com'
  const head = `server {\n    listen 80;\n    listen [::]:80;\n    server_name ${name};\n`
  if (kind === 'proxy') {
    return `${head}
    location / {
        proxy_pass ${target.trim() || 'http://127.0.0.1:3000'};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }
}
`
  }
  if (kind === 'static') {
    return `${head}
    root ${target.trim() || '/var/www/' + name};
    index index.html;

    location / {
        try_files $uri $uri/ =404;
    }
}
`
  }
  if (kind === 'redirect') return `${head}\n    return 301 ${(target.trim() || 'https://www.' + name).replace(/\/$/, '')}$request_uri;\n}\n`
  return ''
}

/** Where a new site's file goes: sites-available/<domain>, or conf.d/<domain>.conf. */
export function newPath(sitesDir: string, domain: string): string {
  const base = domain.trim().toLowerCase().replace(/^\*\./, 'wildcard.').replace(/[^a-z0-9._-]+/g, '-').replace(/^[-.]+|[-.]+$/g, '')
  if (!base) return ''
  return `${sitesDir}/${base}${sitesDir.endsWith('/sites-available') ? '' : '.conf'}`
}

/** "443 ssl http2" → "443" */
export const port = (listen: string) => listen.split(' ')[0].replace(/^.*:(\d+)$/, '$1')
export const ports = (s: NginxSite) => [...new Set(s.listens.map(port))]
export const siteName = (s: NginxSite) => (s.names.length ? (s.names[0] === '_' ? 'catch-all' : s.names[0]) : s.kind === 'stream' ? `stream :${ports(s).join(', ')}` : 'default server')
export const base = (p: string) => p.split('/').pop() ?? p

/** "…in /etc/nginx/conf.d/x.conf:12" → the line, when it is about this file. */
export function errorLine(output: string, file: string): number {
  for (const m of output.matchAll(/ in (\/[^\s:]+):(\d+)/g)) if (m[1] === file || base(m[1]) === base(file)) return Number(m[2])
  return 0
}

/** One sentence for what an apply did. */
export function applyText(r: NginxApply, what: string): { ok: boolean; text: string; detail: string } {
  if (!r.ok) return { ok: false, text: `nginx rejected it, so nothing changed. ${what} was put back as it was.`, detail: r.testOutput }
  if (!r.reloaded) return { ok: true, text: `${what}: saved and tested, but nginx was not reloaded${r.reloadOutput ? '' : ' (it is not running)'}.`, detail: r.reloadOutput }
  return { ok: true, text: `${what}: tested and live.`, detail: '' }
}

export const daysLevel = (d?: number) => (d === undefined ? 'default' : d < 0 ? 'danger' : d < 21 ? 'warning' : 'success')
export const daysText = (d?: number) => (d === undefined ? 'unknown' : d < 0 ? `expired ${-d} d ago` : `${d} d left`)
