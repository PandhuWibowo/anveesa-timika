// Map sources: Kubernetes clusters and cloud accounts (AWS, Tencent Cloud,
// Google Cloud, Azure), read through a CLI on a server or with credentials
// from the vault, and joined into the map. The CLIs are stand-ins on the SSH
// target; the APIs are a local server that checks how each request is signed.
import { chmodSync, mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { scenario, fixture } from '../suite'
import { uniq } from '../fixtures'
import { freshVault, web, createUser, startSsh, ok, eq, fails, waitFor, auditEntries, WORK, Code } from '../lib'

const K8S = { kind: 'List', items: [
  { kind: 'Node', metadata: { name: 'node-a', labels: { 'topology.kubernetes.io/zone': 'us-east-1a' } }, spec: { providerID: 'aws:///us-east-1a/i-0other' }, status: { addresses: [{ type: 'InternalIP', address: '10.0.3.3' }], conditions: [{ type: 'Ready', status: 'True' }], nodeInfo: { kubeletVersion: 'v1.30.2' } } },
  { kind: 'Pod', metadata: { name: 'api-7d9f8c6b5-x2x4z', namespace: 'shop', labels: { app: 'api' }, ownerReferences: [{ kind: 'ReplicaSet', name: 'api-7d9f8c6b5' }] }, spec: { nodeName: 'node-a', containers: [{ image: 'acme/api:2.1', ports: [{ name: 'http', containerPort: 3000 }] }] }, status: { phase: 'Running', podIP: '10.244.1.7' } },
  { kind: 'Service', metadata: { name: 'api', namespace: 'shop' }, spec: { type: 'LoadBalancer', clusterIP: '10.96.0.20', selector: { app: 'api' }, ports: [{ name: 'web', port: 80, targetPort: 'http', nodePort: 31080 }] }, status: { loadBalancer: { ingress: [{ hostname: 'web-1.us-east-1.elb.amazonaws.com' }] } } },
  { kind: 'Ingress', metadata: { name: 'shop', namespace: 'shop' }, spec: { rules: [{ host: 'shop.example.com', http: { paths: [{ path: '/', backend: { service: { name: 'api', port: { number: 80 } } } }] } }] }, status: { loadBalancer: { ingress: [{ ip: '203.0.113.10' }] } } },
] }
const AWS: Record<string, unknown> = {
  'ec2-describe-instances': { Reservations: [{ Instances: [
    { InstanceId: 'i-0app', InstanceType: 't3.large', State: { Name: 'running' }, PrivateIpAddress: '10.0.2.7', SubnetId: 'subnet-priv', VpcId: 'vpc-1', Placement: { AvailabilityZone: 'us-east-1b' }, Tags: [{ Key: 'Name', Value: 'app-1' }] },
    { InstanceId: 'i-0other', InstanceType: 't3.large', State: { Name: 'running' }, PrivateIpAddress: '10.0.3.3', SubnetId: 'subnet-k8s', VpcId: 'vpc-1', Placement: { AvailabilityZone: 'us-east-1a' } },
  ] }] },
  'elbv2-describe-load-balancers': { LoadBalancers: [{ LoadBalancerArn: 'arn:lb/web', LoadBalancerName: 'web', DNSName: 'web-1.us-east-1.elb.amazonaws.com', Scheme: 'internet-facing', Type: 'application', VpcId: 'vpc-1', State: { Code: 'active' } }] },
  'elbv2-describe-target-groups': { TargetGroups: [{ TargetGroupArn: 'arn:tg/app', TargetGroupName: 'app', Port: 8080, LoadBalancerArns: ['arn:lb/web'] }] },
  'elbv2-describe-listeners': { Listeners: [{ Port: 443, Protocol: 'HTTPS', DefaultActions: [{ Type: 'forward', TargetGroupArn: 'arn:tg/app' }] }] },
  'elbv2-describe-target-health': { TargetHealthDescriptions: [{ Target: { Id: 'i-0app', Port: 8080 }, TargetHealth: { State: 'healthy' } }] },
  'ec2-describe-nat-gateways': { NatGateways: [{ NatGatewayId: 'nat-1', State: 'available', SubnetId: 'subnet-pub', VpcId: 'vpc-1', NatGatewayAddresses: [{ PublicIp: '52.9.9.9', PrivateIp: '10.0.1.200' }] }] },
  'ec2-describe-route-tables': { RouteTables: [{ Routes: [{ NatGatewayId: 'nat-1' }], Associations: [{ SubnetId: 'subnet-priv' }] }] },
}
const TENCENT: Record<string, unknown> = {
  'cvm-DescribeInstances': { TotalCount: 1, InstanceSet: [{ InstanceId: 'ins-web', InstanceName: 'web-1', InstanceType: 'S5.MEDIUM4', InstanceState: 'RUNNING', PrivateIpAddresses: ['10.8.1.5'], PublicIpAddresses: ['43.156.1.2'], Placement: { Zone: 'ap-singapore-1' }, VirtualPrivateCloud: { VpcId: 'vpc-1', SubnetId: 'subnet-a' } }] },
  'clb-DescribeLoadBalancers': { LoadBalancerSet: [{ LoadBalancerId: 'lb-1', LoadBalancerName: 'shop', LoadBalancerType: 'OPEN', LoadBalancerVips: ['119.28.1.1'], Status: 1, VpcId: 'vpc-1' }] },
  'clb-DescribeTargets': { Listeners: [{ Protocol: 'HTTPS', Port: 443, Rules: [{ Domain: 'shop.example.com', Url: '/', Targets: [{ InstanceId: 'ins-web', Port: 8080 }] }] }] },
  'vpc-DescribeNatGateways': { NatGatewaySet: [{ NatGatewayId: 'nat-1', NatGatewayName: 'egress', State: 'AVAILABLE', VpcId: 'vpc-1', PublicIpAddressSet: [{ PublicIpAddress: '119.28.9.9' }] }] },
  'vpc-DescribeRouteTables': { RouteTableSet: [{ RouteSet: [{ GatewayType: 'NAT', GatewayId: 'nat-1' }], AssociationSet: [{ SubnetId: 'subnet-a' }] }] },
}
const P = 'https://www.googleapis.com/compute/v1/projects/acme'
const GCP: Record<string, unknown[]> = {
  instances: [{ name: 'web-abcd', id: '111', zone: `${P}/zones/us-central1-a`, machineType: `${P}/zones/us-central1-a/machineTypes/e2-medium`, status: 'RUNNING', networkInterfaces: [{ networkIP: '10.128.0.5', network: `${P}/global/networks/prod` }], metadata: { items: [{ key: 'created-by', value: 'projects/1/zones/us-central1-a/instanceGroupManagers/web-mig' }] } }],
  forwardingRules: [{ name: 'web-https', IPAddress: '34.120.0.1', IPProtocol: 'TCP', portRange: '443-443', loadBalancingScheme: 'EXTERNAL_MANAGED', target: `${P}/global/targetHttpsProxies/web` }],
  targetHttpsProxies: [{ selfLink: `${P}/global/targetHttpsProxies/web`, urlMap: `${P}/global/urlMaps/web` }],
  urlMaps: [{ selfLink: `${P}/global/urlMaps/web`, defaultService: `${P}/global/backendServices/web-bs` }],
  backendServices: [{ selfLink: `${P}/global/backendServices/web-bs`, port: 8080, backends: [{ group: `${P}/zones/us-central1-a/instanceGroups/web-mig` }] }],
  routers: [{ name: 'r', region: `${P}/regions/us-central1`, network: `${P}/global/networks/prod`, nats: [{ name: 'egress', natIps: [`${P}/regions/us-central1/addresses/nat-ip`] }] }],
  addresses: [{ selfLink: `${P}/regions/us-central1/addresses/nat-ip`, address: '35.9.9.9' }],
}
const SUB = '/subscriptions/s1/resourceGroups/prod/providers'
const AZURE: Record<string, unknown[]> = {
  virtualMachines: [{ id: `${SUB}/Microsoft.Compute/virtualMachines/web-1`, name: 'web-1', location: 'southeastasia', properties: { provisioningState: 'Succeeded', hardwareProfile: { vmSize: 'Standard_B2s' } } }],
  networkInterfaces: [{ properties: { virtualMachine: { id: `${SUB}/Microsoft.Compute/virtualMachines/web-1` }, ipConfigurations: [{ properties: { privateIPAddress: '10.1.0.4', subnet: { id: `${SUB}/Microsoft.Network/virtualNetworks/prod/subnets/app` }, loadBalancerBackendAddressPools: [{ id: `${SUB}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web` }] } }] } }],
  publicIPAddresses: [{ id: `${SUB}/Microsoft.Network/publicIPAddresses/shop-ip`, properties: { ipAddress: '20.9.9.9' } }, { id: `${SUB}/Microsoft.Network/publicIPAddresses/nat-ip`, properties: { ipAddress: '20.5.5.5' } }],
  loadBalancers: [{ id: `${SUB}/Microsoft.Network/loadBalancers/shop`, name: 'shop', location: 'southeastasia', sku: { name: 'Standard' }, properties: { provisioningState: 'Succeeded', frontendIPConfigurations: [{ properties: { publicIPAddress: { id: `${SUB}/Microsoft.Network/publicIPAddresses/shop-ip` } } }], backendAddressPools: [{ id: `${SUB}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web` }], loadBalancingRules: [{ name: 'https', properties: { frontendPort: 443, backendPort: 8443, protocol: 'Tcp', backendAddressPool: { id: `${SUB}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web` } } }] } }],
  natGateways: [{ id: `${SUB}/Microsoft.Network/natGateways/egress`, name: 'egress', location: 'southeastasia', properties: { provisioningState: 'Succeeded', publicIpAddresses: [{ id: `${SUB}/Microsoft.Network/publicIPAddresses/nat-ip` }], subnets: [{ id: `${SUB}/Microsoft.Network/virtualNetworks/prod/subnets/app` }] } }],
  applicationGateways: [],
}

/** JSON as the Query API's XML: lists hold <item> (EC2) or <member> (ELB) elements. */
const xml = (v: unknown, tag: string, el: string): string =>
  Array.isArray(v) ? `<${tag}>${v.map((x) => xml(x, el, el)).join('')}</${tag}>`
  : v !== null && typeof v === 'object' ? `<${tag}>${Object.entries(v).map(([k, x]) => xml(x, k, el)).join('')}</${tag}>` : `<${tag}>${String(v)}</${tag}>`

/** The providers' APIs, as far as timika uses them. Remembers how each request was authorised. */
function fakeApi() {
  const seen: Record<string, string> = {}
  const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { 'content-type': 'application/json' } })
  const srv = Bun.serve({ port: 0, async fetch(req) {
    const u = new URL(req.url)
    const auth = req.headers.get('authorization') ?? ''
    const body = req.method === 'POST' ? await req.text() : ''
    // Kubernetes
    if (u.pathname.startsWith('/api/') || u.pathname.startsWith('/apis/')) {
      seen.kubernetes = auth
      if (auth !== 'Bearer k8s-token') return json({ kind: 'Status', message: 'Unauthorized', code: 401 }, 401)
      const kind = { nodes: 'Node', pods: 'Pod', services: 'Service', ingresses: 'Ingress' }[u.pathname.split('/').pop()!]
      // Like the real API: no `kind` on the items of a list.
      return json({ kind: `${kind}List`, items: K8S.items.filter((i) => i.kind === kind).map(({ kind: _, ...rest }) => rest) })
    }
    // Tencent Cloud API 3.0
    if (req.headers.get('x-tc-action')) {
      seen.tencent = `${auth} | region ${req.headers.get('x-tc-region')} | version ${req.headers.get('x-tc-version')}`
      const service = (req.headers.get('host') ?? '').split('.')[0]
      const key = Object.keys(TENCENT).find((k) => k.endsWith(`-${req.headers.get('x-tc-action')}`))
      void service
      return json({ Response: { ...(TENCENT[key ?? ''] as object ?? {}), RequestId: 'r1' } })
    }
    // AWS Query APIs
    if (body.startsWith('Action=')) {
      seen.aws = `${auth} | ${req.headers.get('x-amz-date') ? 'dated' : 'undated'}`
      const action = new URLSearchParams(body).get('Action')!
      const ec2 = ['DescribeInstances', 'DescribeNatGateways', 'DescribeRouteTables'].includes(action)
      const key = Object.keys(AWS).find((k) => k.replace(/-/g, '').toLowerCase().endsWith(action.toLowerCase()))!
      const inner = Object.entries(AWS[key] as object).map(([k, v]) => xml(v, k, ec2 ? 'item' : 'member')).join('')
      return new Response(ec2 ? `<?xml version="1.0"?><${action}Response xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>r</requestId>${inner}</${action}Response>` : `<${action}Response><${action}Result>${inner}</${action}Result></${action}Response>`, { headers: { 'content-type': 'text/xml' } })
    }
    // Google: the token exchange, then aggregated lists
    if (u.pathname === '/token') {
      const jwt = new URLSearchParams(body).get('assertion') ?? ''
      seen.gcp = JSON.stringify(JSON.parse(Buffer.from(jwt.split('.')[1] ?? '', 'base64url').toString() || '{}'))
      return jwt.split('.').length === 3 ? json({ access_token: 'g-token', expires_in: 3600 }) : json({ error_description: 'bad assertion' }, 400)
    }
    if (u.pathname.startsWith('/compute/v1/projects/acme/aggregated/')) {
      if (auth !== 'Bearer g-token') return json({ error: { message: 'no token' } }, 401)
      const name = u.pathname.split('/').pop()!
      return json({ items: { 'zones/us-central1-a': { [name]: GCP[name] ?? [] }, 'zones/empty': { warning: { code: 'NO_RESULTS_ON_PAGE' } } } })
    }
    // Azure: client credentials, then Resource Manager
    if (u.pathname.endsWith('/oauth2/v2.0/token')) {
      const f = new URLSearchParams(body)
      seen.azure = `${u.pathname} | ${f.get('client_id')} | ${f.get('scope')}`
      return f.get('client_secret') === 'az-secret' ? json({ access_token: 'az-token' }) : json({ error_description: 'AADSTS7000215: Invalid client secret provided.' }, 401)
    }
    if (u.pathname.startsWith('/subscriptions/s1/providers/')) {
      if (auth !== 'Bearer az-token') return json({ error: { message: 'no token' } }, 401)
      return json({ value: AZURE[u.pathname.split('/').pop()!] ?? [] })
    }
    return json({ message: `unexpected ${req.method} ${u.pathname}` }, 404)
  } })
  return { url: `http://127.0.0.1:${srv.port}`, seen }
}

const srcs = fixture(async () => {
  const v = await freshVault({ MONITOR_INTERVAL_SECS: '1' })
  const admin = await createUser(v, 'src-admin', ['admin'])
  const c = web(v.node, admin)
  const root = join(WORK, uniq('clis'))
  const bin = join(root, 'bin')
  mkdirSync(bin, { recursive: true })
  const put = (name: string, body: string) => { writeFileSync(join(bin, name), `#!/bin/sh\necho "${name} $*" >> "${root}/calls"\n${body}\n`); chmodSync(join(bin, name), 0o755) }
  const file = (name: string, v: unknown) => writeFileSync(join(root, name), JSON.stringify(v))
  file('kubectl.json', K8S)
  for (const [k, v] of Object.entries(AWS)) file(`aws-${k}.json`, v)
  for (const [k, v] of Object.entries(TENCENT)) file(`tccli-${k}.json`, v)
  for (const [k, v] of Object.entries(GCP)) file(`gcloud-${k}.json`, v)
  for (const [k, v] of Object.entries(AZURE)) file(`az-${k}.json`, { value: v })
  put('kubectl', `cat "${root}/kubectl.json"`)
  put('aws', `f="${root}/aws-$1-$2.json"; if [ -f "$f" ]; then cat "$f"; else echo '{}'; fi`)
  put('tccli', `if [ "$2" = DescribeInstances ] && [ "$4" = ap-nowhere ]; then echo "[TencentCloudSDKException] code:UnsupportedRegion message:The region is not supported" >&2; exit 255; fi\ncat "${root}/tccli-$1-$2.json"`)
  put('gcloud', `n=$(echo "$2" | awk -F- '{ printf "%s", $1; for (i = 2; i <= NF; i++) printf "%s", toupper(substr($i, 1, 1)) substr($i, 2) }'); f="${root}/gcloud-$n.json"; if [ -f "$f" ]; then cat "$f"; else echo '[]'; fi`)
  put('az', `n=$(echo "$5" | sed 's/?.*//; s|.*/||'); cat "${root}/az-$n.json"`)
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('src-host'), env: { PATH: `${bin}:${process.env.PATH}` } })
  // A monitored server that is the AWS instance i-0app (10.0.2.7), with something listening on 8080.
  const now = Math.floor(Date.now() / 1000)
  writeFileSync(join(t.files, '.timika-metrics'), [`now=${now}`, 'os=Ubuntu 24.04', 'kernel=6.8', 'cpus=2', 'model=x', 'uptime=1000', 'stat=cpu  1 0 0 9 0 0 0 0', 'load=0 0 0', 'mem.MemTotal=1000', 'mem.MemAvailable=500',
    'ip=10.0.2.7', 'lsn=tcp|0.0.0.0:8080|node', 'con=|10.0.2.7:40000|10.244.1.7:3000|node', 'con=|10.0.2.7:40001|10.0.1.200:443|node', 'end=1', ''].join('\n'))
  const a = await c.bastion.createAsset({ name: uniq('app'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  const id = a.asset!.id
  await c.monitor.setMonitoring({ assets: [id], enabled: true })
  await waitFor('first reading', async () => (await c.monitor.listSystems({})).systems.find((s) => s.asset === id)?.status === 'up')
  // These scenarios are about the cloud APIs: detect with them.
  await c.monitor.setMapMethod({ method: 'api' })
  return { v, node: v.node, c, id, name: a.asset!.name, root, api: fakeApi() }
})

const counts = (s: { counts: { kind: string; count: number }[] }) => Object.fromEntries(s.counts.map((c) => [c.kind, c.count]))
const add = async (x: Awaited<ReturnType<typeof srcs>>, source: Record<string, unknown>, secret: Record<string, string> = {}) => {
  const s = await x.c.sources.saveSource({ source: { name: uniq('src'), ...source }, secret })
  return x.c.sources.refreshSource({ id: s.id })
}

scenario('Q', 'a cluster and a cloud account, read with kubectl and aws on a server, join the map: the instance is your server, a pod and a NAT gateway get their names', async () => {
  const x = await srcs()
  const k = await add(x, { name: 'k-prod', kind: 'kubernetes', access: 'server', asset: x.id, scope: 'prod-context' })
  eq([k.error, counts(k), k.hasSecret, !!k.readAt], ['', { ingress: 1, node: 1, service: 1, workload: 1 }, false, true], 'the cluster')
  const a = await add(x, { name: 'a-prod', kind: 'aws', access: 'server', asset: x.id, regions: ['us-east-1'] })
  eq([a.error, counts(a)], ['', { lb: 1, nat: 1, vm: 2 }], 'the account')
  const calls = await Bun.file(join(x.root, 'calls')).text()
  ok(calls.includes('kubectl get nodes,pods,services,ingresses.networking.k8s.io --all-namespaces -o json --request-timeout=30s --context prod-context'), `kubectl, read-only, with the context: ${calls}`)
  ok(calls.includes('aws ec2 describe-instances --region us-east-1 --output json --no-cli-pager') && calls.includes('aws elbv2 describe-target-health --target-group-arn arn:tg/app --region us-east-1'), 'aws describe calls per region')

  const m = await waitFor('the sources on the map', async () => { const r = await x.c.monitor.getMap({}); return r.nodes.some((n) => n.kind === 'cluster') && r.nodes.some((n) => n.kind === 'cloud') && r })
  const S = `s:${x.id}`, K = `i:${k.id}`, A = `i:${a.id}`
  const mine = m.edges.filter((e) => [e.from, e.to].some((y) => y.includes(x.id) || y.includes(k.id) || y.includes(a.id)))
  eq(mine.map((e) => `${e.from} -> ${e.to} :${e.port}${e.observed ? ' seen' : ''}${e.declared ? ` ${e.declaredBy}` : ''}`).sort(), [
    `${A}:lb:web -> ${K}:svc:shop/api :80 Kubernetes`,              // the service's load balancer is the AWS one (by DNS name)
    `${A}:lb:web -> p:${x.id}:node :8080 AWS`,                       // a target that is a monitored server: the process listening there
    `${A}:nat:nat-1 -> internet :0 AWS`,
    `${K}:ing:shop/shop -> ${K}:svc:shop/api :80 Kubernetes`,
    `${K}:svc:shop/api -> ${K}:wl:shop/Deployment/api :3000 Kubernetes`,
    `${S} -> ${A}:nat:nat-1 :0 AWS`,                                 // its subnet routes through the NAT gateway
    `internet -> ${A}:lb:web :443 AWS`,
    `internet -> ${K}:ing:shop/shop :80 Kubernetes`,
    `p:${x.id}:node -> ${A}:nat:nat-1 :443 seen`,                    // a connection to an address that is the NAT gateway
    `p:${x.id}:node -> ${K}:wl:shop/Deployment/api :3000 seen`,      // … and to a pod
  ].sort(), 'end to end, across server, cloud and cluster')
  const node = (id: string) => m.nodes.find((n) => n.id === id)!
  ok(node(S).attrs.includes('AWS: a-prod · app-1 · t3.large · us-east-1b') && node(S).attrs.includes('Instance: i-0app'), `the server carries the cloud's facts: ${node(S).attrs}`)
  ok(!node(`${A}:vm:i-0app`), 'and is not drawn twice')
  eq([node(`${A}:vm:i-0other`).kind, node(`${A}:vm:i-0other`).attrs.some((t) => t.startsWith('Kubernetes node: k-prod · node-a'))], ['vm', true], 'an unmonitored instance that is a cluster node')
  eq([node(`${A}:lb:web`).public, node(`${A}:lb:web`).group, node(`${K}:wl:shop/Deployment/api`).scope, node(`${K}:wl:shop/Deployment/api`).state, node(`g:${k.id}`).label], [true, `g:${a.id}`, 'shop', '1 of 1 running', 'k-prod'], 'details')
  const lb = mine.find((e) => e.to === `p:${x.id}:node` && e.declared)!
  eq(lb.note, 'target group app · listener 443 · healthy', 'the target group, listener and health')

  // Someone who is not an administrator sees their servers, not the estate.
  const name = uniq('src-user')
  const u = web(x.node, await createUser(x.v, name, ['ssh']))
  await x.c.bastion.createGrant({ asset: x.id, subjectType: 'user', subject: name })
  const part = await u.monitor.getMap({})
  ok(part.nodes.some((n) => n.id === S) && !part.nodes.some((n) => ['cloud', 'cluster', 'lb', 'nat', 'workload'].includes(n.kind)), 'no cloud or cluster for non-admins')
  await fails(u.sources.listSources({}), Code.PermissionDenied)
  await fails(u.sources.saveSource({ source: { name: 'x', kind: 'aws', access: 'vault', regions: ['us-east-1'] } }), Code.PermissionDenied)
  await fails(u.sources.refreshSource({ id: k.id }), Code.PermissionDenied)
})

scenario('Q', 'credentials in the vault: every provider’s API is called signed the way it expects, and gives the same inventory as its CLI', async () => {
  const x = await srcs()
  const key = Bun.spawnSync(['openssl', 'genpkey', '-algorithm', 'RSA', '-pkeyopt', 'rsa_keygen_bits:2048'], { stderr: 'ignore' }).stdout.toString()
  const sa = JSON.stringify({ type: 'service_account', client_email: 'timika@acme.iam.gserviceaccount.com', private_key: key, token_uri: 'https://oauth2.googleapis.com/token' })
  const k = await add(x, { kind: 'kubernetes', access: 'vault', endpoint: x.api.url }, { token: 'k8s-token' })
  eq([k.error, counts(k), k.hasSecret], ['', { ingress: 1, node: 1, service: 1, workload: 1 }, true], 'kubernetes: a bearer token; kinds added to the bare list items')
  const a = await add(x, { kind: 'aws', access: 'vault', regions: ['us-east-1'], endpoint: x.api.url }, { access_key_id: 'AKIDEXAMPLE', secret_access_key: 'secret' })
  eq([a.error, counts(a)], ['', { lb: 1, nat: 1, vm: 2 }], 'aws: the XML answers read like the CLI’s JSON')
  ok(/^AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE\/\d{8}\/us-east-1\/(ec2|elasticloadbalancing)\/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=[0-9a-f]{64} \| dated$/.test(x.api.seen.aws), `SigV4: ${x.api.seen.aws}`)
  const t = await add(x, { kind: 'tencent', access: 'vault', regions: ['ap-singapore'], endpoint: x.api.url }, { secret_id: 'AKIDtencent', secret_key: 'secret' })
  eq([t.error, counts(t)], ['', { lb: 1, nat: 1, vm: 1 }], 'tencent')
  ok(/^TC3-HMAC-SHA256 Credential=AKIDtencent\/\d{4}-\d\d-\d\d\/(cvm|clb|vpc)\/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature=[0-9a-f]{64} \| region ap-singapore \| version 20\d\d-\d\d-\d\d$/.test(x.api.seen.tencent), `TC3: ${x.api.seen.tencent}`)
  const g = await add(x, { kind: 'gcp', access: 'vault', scope: 'acme', endpoint: x.api.url }, { service_account: sa })
  eq([g.error, counts(g)], ['', { lb: 1, nat: 1, vm: 1 }], 'google: aggregated lists, through proxy and URL map to the group’s instances')
  const claims = JSON.parse(x.api.seen.gcp)
  eq([claims.iss, claims.scope, claims.exp - claims.iat], ['timika@acme.iam.gserviceaccount.com', 'https://www.googleapis.com/auth/compute.readonly', 600], 'a short-lived, read-only signed assertion')
  const z = await add(x, { kind: 'azure', access: 'vault', scope: 's1', endpoint: x.api.url }, { tenant_id: 'tenant-1', client_id: 'app-1', client_secret: 'az-secret' })
  eq([z.error, counts(z)], ['', { lb: 1, nat: 1, vm: 1 }], 'azure')
  eq(x.api.seen.azure, '/tenant-1/oauth2/v2.0/token | app-1 | https://management.azure.com/.default', 'client credentials for Resource Manager')

  // Credentials are write-only, kept when left out, and never in a response or the audit log.
  const listed = (await x.c.sources.listSources({})).sources
  const everything = JSON.stringify(listed) + JSON.stringify(auditEntries(x.node))
  for (const s of ['k8s-token', 'az-secret', 'BEGIN PRIVATE KEY', 'AKIDEXAMPLE']) ok(!everything.includes(s), `${s} is not returned or logged`)
  const renamed = await x.c.sources.saveSource({ source: { ...listed.find((s) => s.id === z.id)!, name: 'azure-renamed' } })
  eq([renamed.name, renamed.hasSecret, (await x.c.sources.refreshSource({ id: z.id })).error], ['azure-renamed', true, ''], 'changed without giving the secret again')
  // A wrong secret is said in the provider's words; the last good inventory stays.
  await x.c.sources.saveSource({ source: { ...listed.find((s) => s.id === z.id)! }, secret: { client_secret: 'wrong' } })
  const bad = await x.c.sources.refreshSource({ id: z.id })
  ok(bad.error.includes('401') && bad.error.includes('Invalid client secret'), `the reason: ${bad.error}`)
  const wrong = await add(x, { kind: 'kubernetes', access: 'vault', endpoint: x.api.url }, { token: 'nope' })
  ok(wrong.error.includes('401') && wrong.error.includes('Unauthorized') && wrong.counts.length === 0, `kubernetes says no: ${wrong.error}`)
  for (const s of [k, a, t, g, z, wrong]) await x.c.sources.deleteSource({ id: s.id })
})

scenario('Q', 'tccli, gcloud and az on a server give the same inventory; a region that fails is noted, a missing CLI is named', async () => {
  const x = await srcs()
  const t = await add(x, { kind: 'tencent', access: 'server', asset: x.id, regions: ['ap-singapore', 'ap-nowhere'] })
  eq([t.error, counts(t)], ['', { lb: 1, nat: 1, vm: 1 }], 'tencent: the region that works')
  ok(t.notes.length === 1 && t.notes[0].startsWith('ap-nowhere: ') && t.notes[0].includes('The region is not supported'), `the other is noted, in the CLI's words: ${t.notes}`)
  const g = await add(x, { kind: 'gcp', access: 'server', asset: x.id, scope: 'acme' })
  eq([g.error, counts(g)], ['', { lb: 1, nat: 1, vm: 1 }], 'google')
  const z = await add(x, { kind: 'azure', access: 'server', asset: x.id, scope: 's1' })
  eq([z.error, counts(z)], ['', { lb: 1, nat: 1, vm: 1 }], 'azure')
  const calls = await Bun.file(join(x.root, 'calls')).text()
  ok(calls.includes('tccli cvm DescribeInstances --region ap-singapore --output json --Limit 100 --Offset 0') && calls.includes('tccli clb DescribeTargets --region ap-singapore --output json --LoadBalancerId lb-1'), 'tccli')
  ok(calls.includes('gcloud compute forwarding-rules list --project acme --format=json --quiet'), 'gcloud')
  ok(calls.includes('az rest --method get --url https://management.azure.com/subscriptions/s1/providers/Microsoft.Network/natGateways?api-version=2023-09-01 --output json'), 'az rest: the same documents as the API')
  const m = await x.c.monitor.getMap({})
  const e = (from: string, to: string) => m.edges.some((y) => y.from === from && y.to === to && y.declared)
  ok(e('internet', `i:${t.id}:lb:lb-1`) && e(`i:${t.id}:lb:lb-1`, `i:${t.id}:vm:ins-web`) && e(`i:${t.id}:vm:ins-web`, `i:${t.id}:nat:nat-1`), 'tencent: internet → CLB → CVM → NAT gateway')
  ok(e(`i:${g.id}:lb:web-https`, `i:${g.id}:vm:us-central1-a/web-abcd`) && e(`i:${g.id}:vm:us-central1-a/web-abcd`, `i:${g.id}:nat:us-central1/egress`), 'google: load balancer → instance → Cloud NAT')
  ok(e(`i:${z.id}:lb:prod/shop`, `i:${z.id}:vm:prod/web-1`) && e(`i:${z.id}:vm:prod/web-1`, `i:${z.id}:nat:prod/egress`), 'azure: load balancer → VM → NAT gateway')

  // A server without the CLI.
  const bare = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('bare-src'), env: { PATH: '/bin:/usr/bin' } })
  writeFileSync(join(bare.files, '.timika-metrics'), `now=${Math.floor(Date.now() / 1000)}\nos=x\ncpus=1\nstat=cpu  1 0 0 9 0 0 0 0\nmem.MemTotal=1000\nmem.MemAvailable=500\nend=1\n`)
  const b = await x.c.bastion.createAsset({ name: uniq('bare'), host: '127.0.0.1', port: bare.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  const un = await add(x, { kind: 'tencent', access: 'server', asset: b.asset!.id, regions: ['ap-singapore'] })
  ok(un.error.includes('not monitored'), `only through a monitored server: ${un.error}`)
  await x.c.monitor.setMonitoring({ assets: [b.asset!.id], enabled: true })
  const missing = await x.c.sources.refreshSource({ id: un.id })
  ok(missing.error.includes('`tccli` is not installed on that server') && missing.counts.length === 0, `named: ${missing.error}`)
  for (const s of [t, g, z, missing]) await x.c.sources.deleteSource({ id: s.id })
  await x.c.monitor.setMonitoring({ assets: [b.asset!.id], enabled: false })
  await bare.stop()
})

scenario('Q', 'a server whose kubectl reaches a cluster brings that cluster onto the map by itself — once: removing it is final', async () => {
  const x = await srcs()
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('k8s-host'), env: { PATH: `${join(x.root, 'bin')}:${process.env.PATH}` } })
  const now = Math.floor(Date.now() / 1000)
  writeFileSync(join(t.files, '.timika-metrics'), `now=${now}\nos=x\ncpus=1\nstat=cpu  1 0 0 9 0 0 0 0\nmem.MemTotal=1000\nmem.MemAvailable=500\nip=10.0.3.3\nend=1\n`)
  writeFileSync(join(t.files, '.timika-cloud'), 'gw=10.0.3.1\nk8s=1\nend=1\n')
  const a = await x.c.bastion.createAsset({ name: uniq('cp'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })
  await x.c.monitor.setMonitoring({ assets: [a.asset!.id], enabled: true })
  const found = await waitFor('the cluster, added and read', async () => { const s = (await x.c.sources.listSources({})).sources.find((y) => y.asset === a.asset!.id); return s?.readAt && s })
  eq([found.name, found.kind, found.access, found.error, counts(found).workload], [`${a.asset!.name} cluster`, 'kubernetes', 'server', '', 1], 'nobody added it')
  const m = await x.c.monitor.getMap({})
  ok(m.nodes.some((n) => n.id === `g:${found.id}` && n.kind === 'cluster') && m.nodes.find((n) => n.id === `s:${a.asset!.id}`)!.attrs.some((y) => y.startsWith('Kubernetes node: ')), 'on the map, and the server is recognised as its node')
  await x.c.sources.deleteSource({ id: found.id })
  await new Promise((r) => setTimeout(r, 2500))
  ok(!(await x.c.sources.listSources({})).sources.some((y) => y.asset === a.asset!.id), 'removed stays removed')
  await x.c.monitor.setMonitoring({ assets: [a.asset!.id], enabled: false })
  await t.stop()
})

scenario('Q', 'detect with the VMs only, or with the cloud API: the switch decides whether cloud accounts are on the map; the clouds the servers run in are offered, prefilled', async () => {
  const x = await srcs()
  const t = await startSsh({ user: 'ops', password: 'ops-pass', hostKeyName: uniq('how-host') })
  writeFileSync(join(t.files, '.timika-metrics'), `now=${Math.floor(Date.now() / 1000)}\nos=x\ncpus=1\nstat=cpu  1 0 0 9 0 0 0 0\nmem.MemTotal=1000\nmem.MemAvailable=500\nip=10.9.9.9\nend=1\n`)
  writeFileSync(join(t.files, '.timika-cloud'), 'cloud=tencent\nid=ins-how\nregion=ap-singapore\nzone=ap-singapore-2\nprivate=10.9.9.9\nvpc=vpc-how\nk8s=0\nend=1\n')
  const asset = (await x.c.bastion.createAsset({ name: uniq('how'), host: '127.0.0.1', port: t.port, accounts: [{ username: 'ops', password: 'ops-pass' }], test: false })).asset!
  await x.c.monitor.setMonitoring({ assets: [asset.id], enabled: true })
  const cloudOf = (m: { detected: { provider: string; regions: string[]; servers: string[]; connected: boolean }[] }) => m.detected.find((d) => d.provider === 'tencent')
  const m1 = await waitFor('the cloud the server is in', async () => { const r = await x.c.monitor.getMap({}); return cloudOf(r)?.servers.includes(asset.name) && r })
  eq([m1.method, cloudOf(m1)!.regions, cloudOf(m1)!.connected], ['api', ['ap-singapore'], false], 'found by the server itself; no key for it yet')
  const aws = await add(x, { name: 'aws-how', kind: 'aws', access: 'server', asset: x.id, regions: ['us-east-1'] })
  const k8s = await add(x, { name: 'k8s-how', kind: 'kubernetes', access: 'server', asset: x.id })
  const on = (m: { nodes: { id: string }[] }) => [aws.id, k8s.id].map((id) => m.nodes.some((n) => n.id === `g:${id}`))
  eq(on(await x.c.monitor.getMap({})), [true, true], 'Cloud API: the account and the cluster')
  eq((await x.c.monitor.setMapMethod({ method: 'vm' })).method, 'vm', 'switched')
  const vm = await x.c.monitor.getMap({})
  eq([vm.method, ...on(vm)], ['vm', false, true], 'VMs only: no cloud account on the map; the cluster stays (it is read through a server, not with a cloud key)')
  ok(vm.nodes.some((n) => n.id === 'g:auto:tencent:vpc-how'), 'what the servers tell is there in both')
  const tc = await add(x, { name: 'tc-how', kind: 'tencent', access: 'server', asset: x.id, regions: ['ap-singapore'] })
  eq(cloudOf(await x.c.monitor.getMap({}))!.connected, true, 'a key for that cloud now exists')
  await x.c.monitor.setMapMethod({ method: 'api' })
  ok((await x.c.monitor.getMap({})).nodes.find((n) => n.id === 'g:auto:tencent:vpc-how')!.attrs.includes('Load balancers and NAT gateways: from the Tencent Cloud API (tc-how)'), 'with the API nothing about them is guessed')
  await fails(x.c.monitor.setMapMethod({ method: 'magic' }), Code.InvalidArgument, 'unknown method')
  const u = web(x.node, await createUser(x.v, uniq('how-user'), ['ssh']))
  await fails(u.monitor.setMapMethod({ method: 'vm' }), Code.PermissionDenied)
  ok(JSON.stringify(auditEntries(x.node)).includes('map detection: vm'), 'audited')
  for (const s of [aws, k8s, tc]) await x.c.sources.deleteSource({ id: s.id })
  await x.c.monitor.setMonitoring({ assets: [asset.id], enabled: false })
  await t.stop()
})

scenario('Q', 'what a source needs is checked; changing one reads it again; removing one takes it off the map with its credentials', async () => {
  const x = await srcs()
  const bad = (source: Record<string, unknown>, secret: Record<string, string>, msg: string) => fails(x.c.sources.saveSource({ source: { name: 'x', ...source }, secret }), Code.InvalidArgument, msg)
  await bad({ kind: 'digitalocean', access: 'vault' }, {}, 'unknown kind')
  await bad({ kind: 'aws', access: 'vault' }, {}, 'regions')
  await bad({ kind: 'aws', access: 'vault', regions: ['us-east-1; rm -rf /'] }, {}, 'a region')
  await bad({ kind: 'aws', access: 'vault', regions: ['us-east-1'] }, {}, '`access_key_id` is needed')
  await bad({ kind: 'aws', access: 'vault', regions: ['us-east-1'] }, { access_key_id: 'a', secret_access_key: 'b', password: 'c' }, 'not a credential')
  await bad({ kind: 'aws', access: 'server', regions: ['us-east-1'] }, {}, 'choose the server')
  await bad({ kind: 'gcp', access: 'server', asset: x.id }, {}, 'project')
  await bad({ kind: 'azure', access: 'server', asset: x.id, scope: 'a b' }, {}, 'subscription')
  await bad({ kind: 'kubernetes', access: 'vault' }, { token: 't' }, 'API server')
  await bad({ kind: 'kubernetes', access: 'vault', endpoint: 'ftp://x' }, { token: 't' }, 'API server')
  await bad({ kind: 'kubernetes', access: 'server', asset: x.id, scope: '--kubeconfig=/etc/shadow' }, {}, 'context')
  await bad({ name: '', kind: 'aws', access: 'vault', regions: ['us-east-1'] }, {}, 'name')

  const s = await x.c.sources.saveSource({ source: { name: 'k-temp', kind: 'kubernetes', access: 'server', asset: x.id } })
  // The collecting instance reads a new source by itself.
  const read = await waitFor('read in the background', async () => { const r = (await x.c.sources.listSources({})).sources.find((y) => y.id === s.id); return r?.readAt && r })
  eq(counts(read).workload, 1, 'read without asking')
  await fails(x.c.sources.saveSource({ source: { ...read, kind: 'aws', regions: ['us-east-1'] } }), Code.InvalidArgument, 'kind can')
  ok((await x.c.monitor.getMap({})).nodes.some((n) => n.id === `g:${s.id}`), 'on the map')
  await x.c.sources.deleteSource({ id: s.id })
  ok(!(await x.c.monitor.getMap({})).nodes.some((n) => n.id.includes(s.id)), 'gone from the map')
  await fails(x.c.sources.refreshSource({ id: s.id }), Code.NotFound)
  await fails(x.c.sources.deleteSource({ id: s.id }), Code.NotFound)
  const raw = JSON.stringify(auditEntries(x.node))
  ok(raw.includes('source k-temp (kubernetes)') && raw.includes(`source ${s.id}`), 'audited')
})
