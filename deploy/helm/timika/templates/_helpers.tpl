{{- define "timika.fullname" -}}
{{- printf "%s-timika" .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "timika.labels" -}}
app.kubernetes.io/name: timika
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "timika.selector" -}}
app.kubernetes.io/name: timika
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "timika.scheme" -}}
{{- if .Values.tls.enabled -}}https{{- else -}}http{{- end -}}
{{- end -}}

{{/* Stable per-pod DNS name through the headless service. */}}
{{- define "timika.podHost" -}}
{{- printf "%s.%s-internal.%s.svc" .pod (include "timika.fullname" .ctx) .ctx.Release.Namespace -}}
{{- end -}}

{{/* retry_join list: every pod's API address. */}}
{{- define "timika.retryJoin" -}}
{{- $ctx := . -}}
{{- $out := list -}}
{{- range $i := until (int .Values.replicas) -}}
{{- $pod := printf "%s-%d" (include "timika.fullname" $ctx) $i -}}
{{- $out = append $out (printf "%s://%s:8200" (include "timika.scheme" $ctx) (include "timika.podHost" (dict "pod" $pod "ctx" $ctx))) -}}
{{- end -}}
{{- join "," $out -}}
{{- end -}}

{{/* Env shared by both workload kinds. */}}
{{- define "timika.tlsEnv" -}}
{{- if .Values.tls.enabled }}
- { name: TLS_CERT_FILE, value: /tls/tls.crt }
- { name: TLS_KEY_FILE, value: /tls/tls.key }
- { name: TLS_CA_FILE, value: /tls/ca.crt }
{{- end }}
{{- end -}}

{{/* Auto-unseal env. */}}
{{- define "timika.sealEnv" -}}
- { name: SEAL_TYPE, value: {{ .Values.seal.type | quote }} }
{{- if eq .Values.seal.type "transit" }}
- { name: SEAL_TRANSIT_ADDR, value: {{ required "seal.transit.addr is required" .Values.seal.transit.addr | quote }} }
- { name: SEAL_TRANSIT_KEY, value: {{ .Values.seal.transit.key | quote }} }
- { name: SEAL_TRANSIT_MOUNT, value: {{ .Values.seal.transit.mount | quote }} }
- { name: SEAL_TRANSIT_TOKEN_FILE, value: /seal/transit/token }
{{- with .Values.seal.transit.namespace }}
- { name: SEAL_TRANSIT_NAMESPACE, value: {{ . | quote }} }
{{- end }}
{{- if .Values.seal.transit.caSecret }}
- { name: SEAL_TRANSIT_CA_FILE, value: /seal/transit-ca/ca.crt }
{{- end }}
{{- else if eq .Values.seal.type "awskms" }}
- { name: SEAL_AWSKMS_KEY_ID, value: {{ required "seal.awskms.keyId is required" .Values.seal.awskms.keyId | quote }} }
- { name: SEAL_AWSKMS_REGION, value: {{ required "seal.awskms.region is required" .Values.seal.awskms.region | quote }} }
{{- with .Values.seal.awskms.endpoint }}
- { name: SEAL_AWSKMS_ENDPOINT, value: {{ . | quote }} }
{{- end }}
{{- else if eq .Values.seal.type "static" }}
- { name: SEAL_STATIC_KEY_FILE, value: /seal/static/key }
{{- end }}
{{- end -}}

{{/* Secret-backed mounts: TLS + auto-unseal credentials. */}}
{{- define "timika.secretMounts" -}}
- { name: logs, mountPath: /var/log/timika }
{{- if .Values.tls.enabled }}
- { name: tls, mountPath: /tls, readOnly: true }
{{- end }}
{{- if eq .Values.seal.type "transit" }}
- { name: seal-transit, mountPath: /seal/transit, readOnly: true }
{{- if .Values.seal.transit.caSecret }}
- { name: seal-transit-ca, mountPath: /seal/transit-ca, readOnly: true }
{{- end }}
{{- else if eq .Values.seal.type "static" }}
- { name: seal-static, mountPath: /seal/static, readOnly: true }
{{- end }}
{{- end -}}

{{- define "timika.secretVolumes" -}}
{{- if .Values.tls.enabled }}
- name: tls
  secret: { secretName: {{ .Values.tls.secretName }} }
{{- end }}
{{- if eq .Values.seal.type "transit" }}
- name: seal-transit
  secret: { secretName: {{ required "seal.transit.tokenSecret is required" .Values.seal.transit.tokenSecret }} }
{{- if .Values.seal.transit.caSecret }}
- name: seal-transit-ca
  secret: { secretName: {{ .Values.seal.transit.caSecret }} }
{{- end }}
{{- else if eq .Values.seal.type "static" }}
- name: seal-static
  secret: { secretName: {{ required "seal.static.keySecret is required" .Values.seal.static.keySecret }} }
{{- end }}
{{- end -}}

{{- define "timika.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{ default (include "timika.fullname" .) .Values.serviceAccount.name }}
{{- else -}}
{{ default "default" .Values.serviceAccount.name }}
{{- end -}}
{{- end -}}

{{/* Operational + audit logging env. Empty values disable (the image defaults are on). */}}
{{- define "timika.logEnv" -}}
- { name: LOG_LEVEL, value: {{ .Values.logging.level | quote }} }
- { name: LOG_FORMAT, value: {{ .Values.logging.format | quote }} }
- { name: LOG_DIR, value: {{ ternary "/var/log/timika" "" .Values.logging.file.enabled | quote }} }
- { name: LOG_ROTATION, value: {{ .Values.logging.file.rotation | quote }} }
- { name: LOG_MAX_FILES, value: {{ .Values.logging.file.maxFiles | quote }} }
- { name: AUDIT_FILE, value: {{ ternary "/var/log/timika/audit-{instance}.log" "" .Values.logging.audit.file | quote }} }
- { name: AUDIT_FILE_MAX_MB, value: {{ .Values.logging.audit.maxMb | quote }} }
- { name: AUDIT_FILE_MAX_FILES, value: {{ .Values.logging.audit.maxFiles | quote }} }
- { name: AUDIT_FILE_FSYNC, value: {{ .Values.logging.audit.fsync | quote }} }
- { name: AUDIT_SYSLOG, value: {{ .Values.logging.audit.syslog | quote }} }
- { name: AUDIT_FAIL_CLOSED, value: {{ .Values.logging.audit.failClosed | quote }} }
{{- end -}}

{{/* Sign-in page settings. */}}
{{- define "timika.loginEnv" -}}
- { name: LOGIN_POLICY, value: {{ .Values.login.policy | quote }} }
{{- with .Values.login.banner }}
- { name: LOGIN_BANNER, value: {{ . | quote }} }
{{- end }}
{{- with .Values.login.bannerZh }}
- { name: LOGIN_BANNER_ZH, value: {{ . | quote }} }
{{- end }}
{{- with .Values.login.privacyNoticeUrl }}
- { name: PRIVACY_NOTICE_URL, value: {{ . | quote }} }
{{- end }}
- { name: CAPTCHA_PROVIDER, value: {{ .Values.login.captcha.provider | quote }} }
- { name: CAPTCHA_FALLBACK, value: {{ .Values.login.captcha.fallback | quote }} }
- { name: CAPTCHA_SITE_KEY, value: {{ .Values.login.captcha.siteKey | quote }} }
- { name: CAPTCHA_RECAPTCHA_DOMAIN, value: {{ .Values.login.captcha.recaptchaDomain | quote }} }
- { name: TRUST_X_FORWARDED_FOR, value: {{ .Values.login.trustXForwardedFor | quote }} }
{{- end -}}
