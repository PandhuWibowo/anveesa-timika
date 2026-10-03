//! Kubernetes: nodes, workloads (pods grouped by what owns them), services and
//! ingresses, and how they forward to each other. Through `kubectl` on a
//! server, or the API server with a service-account token from the vault.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::fetch::Via;
use super::{list, num, push, s, Inventory, Item, Source};
use crate::error::AppResult;

/// (kind, API path)
const LISTS: [(&str, &str); 4] = [("Node", "/api/v1/nodes"), ("Pod", "/api/v1/pods"), ("Service", "/api/v1/services"), ("Ingress", "/apis/networking.k8s.io/v1/ingresses")];

pub async fn collect(via: &Via, src: &Source) -> AppResult<Inventory> {
    let items = if via.is_cli() {
        let mut args = vec!["get", "nodes,pods,services,ingresses.networking.k8s.io", "--all-namespaces", "-o", "json", "--request-timeout=30s"];
        if !src.scope.is_empty() {
            args.extend(["--context", &src.scope]);
        }
        via.cli("kubectl", &args).await?
    } else {
        // The API's lists leave `kind` out of each item; kubectl adds it.
        let token = match via {
            Via::Api { secret, .. } => secret.get("token").cloned().unwrap_or_default(),
            _ => String::new(),
        };
        let mut all = Vec::new();
        for (kind, path) in LISTS {
            let page = via.get_json(&format!("kubernetes {kind}s"), &format!("{}{path}?limit=3000", src.endpoint), &token).await?;
            for mut i in list(&page, "items").into_iter().cloned() {
                i["kind"] = json!(kind);
                all.push(i);
            }
        }
        json!({ "items": all })
    };
    Ok(parse(&items))
}

fn labels(v: &Value) -> BTreeMap<String, String> {
    v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default()
}

/// What a pod belongs to: (kind, name). A ReplicaSet's pods belong to its Deployment.
fn owner(pod: &Value) -> (String, String) {
    let name = s(&pod["metadata"], "name");
    let Some(o) = list(&pod["metadata"], "ownerReferences").into_iter().next() else { return ("Pod".into(), name) };
    let (kind, oname) = (s(o, "kind"), s(o, "name"));
    let cut = |n: &str| n.rsplit_once('-').map(|x| x.0.to_string()).unwrap_or_else(|| n.to_string());
    match kind.as_str() {
        "ReplicaSet" => ("Deployment".into(), cut(&oname)),
        "Job" if oname.rsplit_once('-').is_some_and(|x| x.1.chars().all(|c| c.is_ascii_digit())) => ("CronJob".into(), cut(&oname)),
        _ => (kind, oname),
    }
}

struct Workload {
    item: Item,
    labels: BTreeMap<String, String>,
    /// container port names → numbers
    named: BTreeMap<String, u16>,
    pods: u32,
    running: u32,
}

pub fn parse(v: &Value) -> Inventory {
    let mut inv = Inventory::default();
    let all = list(v, "items");
    let of = |kind: &'static str| all.iter().copied().filter(move |i| s(i, "kind") == kind);

    for n in of("Node") {
        let (meta, status) = (&n["metadata"], &n["status"]);
        let mut item = Item { id: format!("node:{}", s(meta, "name")), kind: "node".into(), name: s(meta, "name"), ..Default::default() };
        for a in list(status, "addresses") {
            match s(a, "type").as_str() {
                "InternalIP" | "ExternalIP" => push(&mut item.ips, s(a, "address")),
                _ => push(&mut item.refs, s(a, "address")),
            }
        }
        push(&mut item.refs, s(&n["spec"], "providerID"));
        // "aws:///us-east-1a/i-0abc" → the instance id a cloud source knows it by.
        push(&mut item.refs, super::tail(&s(&n["spec"], "providerID")).to_string());
        let ready = list(status, "conditions").into_iter().find(|c| s(c, "type") == "Ready").map(|c| s(c, "status") == "True");
        item.state = match ready { Some(true) => "Ready".into(), Some(false) => "NotReady".into(), None => String::new() };
        item.detail = [s(&status["nodeInfo"], "kubeletVersion"), s(&meta["labels"], "node.kubernetes.io/instance-type")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
        item.scope = s(&meta["labels"], "topology.kubernetes.io/zone");
        inv.items.push(item);
    }

    // Pods, grouped by what owns them.
    let mut workloads: BTreeMap<(String, String, String), Workload> = BTreeMap::new();
    for p in of("Pod") {
        let (meta, spec, status) = (&p["metadata"], &p["spec"], &p["status"]);
        let ns = s(meta, "namespace");
        let (kind, name) = owner(p);
        let w = workloads.entry((ns.clone(), kind.clone(), name.clone())).or_insert_with(|| Workload {
            item: Item { id: format!("wl:{ns}/{kind}/{name}"), kind: "workload".into(), name: name.clone(), scope: ns.clone(), ..Default::default() },
            labels: labels(&meta["labels"]),
            named: BTreeMap::new(),
            pods: 0,
            running: 0,
        });
        w.pods += 1;
        let running = s(status, "phase") == "Running";
        w.running += running as u32;
        // A pod on the host's network has the node's address, which is not its own.
        if running && spec["hostNetwork"] != json!(true) && w.item.ips.len() < 200 {
            push(&mut w.item.ips, s(status, "podIP"));
        }
        let mut nodes: Vec<String> = w.item.attrs.iter().filter(|a| a.0 == "Nodes").flat_map(|a| a.1.split(", ").map(String::from)).collect();
        push(&mut nodes, s(spec, "nodeName"));
        w.item.attrs.retain(|a| a.0 != "Nodes");
        w.item.attrs.push(("Nodes".into(), nodes.join(", ")));
        for c in list(spec, "containers") {
            if w.item.detail.is_empty() {
                w.item.detail = s(c, "image");
            }
            for port in list(c, "ports") {
                let n = num(port, "containerPort");
                let entry = (n, s(port, "protocol").to_lowercase().replace("tcp", "tcp"), s(port, "name"));
                if n > 0 && !w.item.ports.iter().any(|x| x.0 == n) {
                    w.item.ports.push(if entry.1.is_empty() { (entry.0, "tcp".into(), entry.2) } else { entry });
                }
                if !s(port, "name").is_empty() {
                    w.named.insert(s(port, "name"), n);
                }
            }
        }
        // Labels every pod of the workload shares.
        let mine = labels(&meta["labels"]);
        w.labels.retain(|k, v| mine.get(k) == Some(v));
    }
    for w in workloads.values_mut() {
        w.item.state = format!("{} of {} running", w.running, w.pods);
        let kind = w.item.id.split('/').nth(1).unwrap_or_default().to_string();
        w.item.attrs.insert(0, ("Kind".into(), kind));
        w.item.attrs.insert(1, ("Pods".into(), w.pods.to_string()));
    }

    for svc in of("Service") {
        let (meta, spec) = (&svc["metadata"], &svc["spec"]);
        let (ns, name) = (s(meta, "namespace"), s(meta, "name"));
        let id = format!("svc:{ns}/{name}");
        let kind = s(spec, "type");
        let mut item = Item { id: id.clone(), kind: "service".into(), name, scope: ns.clone(), detail: if kind.is_empty() { "ClusterIP".into() } else { kind.clone() }, ..Default::default() };
        let cluster_ip = s(spec, "clusterIP");
        if cluster_ip != "None" {
            push(&mut item.ips, cluster_ip);
        }
        for ip in list(spec, "externalIPs") {
            push(&mut item.ips, ip.as_str().unwrap_or_default());
        }
        let selector = labels(&spec["selector"]);
        let targets: Vec<&Workload> = if selector.is_empty() { Vec::new() } else { workloads.values().filter(|w| w.item.scope == ns && selector.iter().all(|(k, v)| w.labels.get(k) == Some(v))).collect() };
        for p in list(spec, "ports") {
            let port = num(p, "port");
            let node_port = num(p, "nodePort");
            let target = &p["targetPort"];
            item.ports.push((port, s(p, "protocol").to_lowercase(), [s(p, "name"), if node_port > 0 { format!("node port {node_port}") } else { String::new() }].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ")));
            if node_port > 0 {
                item.node_ports.push(node_port);
            }
            for w in &targets {
                // A named target port is looked up in the pods' containers.
                let to = match target {
                    Value::String(n) => w.named.get(n).copied().unwrap_or(port),
                    _ => Some(num(p, "targetPort")).filter(|x| *x > 0).unwrap_or(port),
                };
                inv.link(format!("id:{id}"), format!("id:{}", w.item.id), to, if to != port { format!("service port {port} → {to}") } else { String::new() });
            }
        }
        // What the cloud gave a LoadBalancer service: an address or a DNS name.
        for ing in list(&svc["status"]["loadBalancer"], "ingress") {
            let at = [s(ing, "ip"), s(ing, "hostname")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            if at.is_empty() {
                continue;
            }
            // Shown, not registered: it is the load balancer's address, not the service's.
            item.attrs.push(("Load balancer".into(), at.clone()));
            item.public = true;
            for p in item.ports.clone() {
                inv.link(format!("ext:{at}"), format!("id:{id}"), p.0, "load balancer of the service");
            }
        }
        inv.items.push(item);
    }

    for ing in of("Ingress") {
        let (meta, spec) = (&ing["metadata"], &ing["spec"]);
        let (ns, name) = (s(meta, "namespace"), s(meta, "name"));
        let id = format!("ing:{ns}/{name}");
        let tls = !list(spec, "tls").is_empty();
        let mut item = Item { id: id.clone(), kind: "ingress".into(), name, scope: ns.clone(), ports: vec![(if tls { 443 } else { 80 }, "tcp".into(), if tls { "TLS".into() } else { String::new() })], ..Default::default() };
        let class = [s(spec, "ingressClassName"), s(&meta["annotations"], "kubernetes.io/ingress.class")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
        if !class.is_empty() {
            item.attrs.push(("Class".into(), class));
        }
        let mut hosts = Vec::new();
        let backend = |b: &Value, note: String, inv: &mut Inventory| {
            let svc = s(&b["service"], "name");
            if svc.is_empty() {
                return;
            }
            // A named service port is the service's port of that name.
            let port = match num(&b["service"]["port"], "number") {
                0 => inv.items.iter().find(|i| i.id == format!("svc:{ns}/{svc}")).and_then(|i| i.ports.iter().find(|p| p.2.split(" · ").next() == Some(&s(&b["service"]["port"], "name"))).map(|p| p.0)).unwrap_or(0),
                n => n,
            };
            inv.link(format!("id:{id}"), format!("id:svc:{ns}/{svc}"), port, note);
        };
        backend(&spec["defaultBackend"], "default backend".into(), &mut inv);
        for rule in list(spec, "rules") {
            let host = s(rule, "host");
            push(&mut hosts, host.clone());
            for path in list(&rule["http"], "paths") {
                backend(&path["backend"], format!("{}{}", if host.is_empty() { "*" } else { &host }, s(path, "path")), &mut inv);
            }
        }
        item.detail = hosts.join(", ");
        for lb in list(&ing["status"]["loadBalancer"], "ingress") {
            let at = [s(lb, "ip"), s(lb, "hostname")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
            if !at.is_empty() {
                item.attrs.push(("Load balancer".into(), at.clone()));
                item.public = true;
                inv.link(format!("ext:{at}"), format!("id:{id}"), item.ports[0].0, "load balancer of the ingress");
            }
        }
        inv.items.push(item);
    }
    inv.items.extend(workloads.into_values().map(|w| w.item));
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> Value {
        json!({ "kind": "List", "items": [
            { "kind": "Node", "metadata": { "name": "node-a", "labels": { "node.kubernetes.io/instance-type": "t3.large", "topology.kubernetes.io/zone": "us-east-1a" } }, "spec": { "providerID": "aws:///us-east-1a/i-0abc" },
              "status": { "addresses": [{ "type": "InternalIP", "address": "10.0.1.5" }, { "type": "Hostname", "address": "ip-10-0-1-5.ec2.internal" }], "conditions": [{ "type": "Ready", "status": "True" }], "nodeInfo": { "kubeletVersion": "v1.30.2" } } },
            { "kind": "Pod", "metadata": { "name": "api-7d9f8c6b5-x2x4z", "namespace": "shop", "labels": { "app": "api", "pod-template-hash": "7d9f8c6b5" }, "ownerReferences": [{ "kind": "ReplicaSet", "name": "api-7d9f8c6b5" }] },
              "spec": { "nodeName": "node-a", "containers": [{ "image": "acme/api:2.1", "ports": [{ "name": "http", "containerPort": 3000, "protocol": "TCP" }] }] }, "status": { "phase": "Running", "podIP": "10.244.1.7", "hostIP": "10.0.1.5" } },
            { "kind": "Pod", "metadata": { "name": "api-7d9f8c6b5-q8w2e", "namespace": "shop", "labels": { "app": "api", "pod-template-hash": "7d9f8c6b5" }, "ownerReferences": [{ "kind": "ReplicaSet", "name": "api-7d9f8c6b5" }] },
              "spec": { "nodeName": "node-b", "containers": [{ "image": "acme/api:2.1", "ports": [{ "name": "http", "containerPort": 3000 }] }] }, "status": { "phase": "Pending" } },
            { "kind": "Pod", "metadata": { "name": "db-0", "namespace": "shop", "labels": { "app": "db" }, "ownerReferences": [{ "kind": "StatefulSet", "name": "db" }] },
              "spec": { "nodeName": "node-a", "containers": [{ "image": "postgres:16", "ports": [{ "containerPort": 5432 }] }] }, "status": { "phase": "Running", "podIP": "10.244.1.9" } },
            { "kind": "Pod", "metadata": { "name": "kube-proxy-abc", "namespace": "kube-system", "labels": { "k8s-app": "kube-proxy" }, "ownerReferences": [{ "kind": "DaemonSet", "name": "kube-proxy" }] },
              "spec": { "nodeName": "node-a", "hostNetwork": true, "containers": [{ "image": "kube-proxy:v1.30" }] }, "status": { "phase": "Running", "podIP": "10.0.1.5" } },
            { "kind": "Pod", "metadata": { "name": "backup-28800000-abcde", "namespace": "shop", "labels": { "job": "backup" }, "ownerReferences": [{ "kind": "Job", "name": "backup-28800000" }] },
              "spec": { "nodeName": "node-a", "containers": [{ "image": "acme/backup:1" }] }, "status": { "phase": "Succeeded" } },
            { "kind": "Service", "metadata": { "name": "api", "namespace": "shop" }, "spec": { "type": "LoadBalancer", "clusterIP": "10.96.0.20", "selector": { "app": "api" }, "ports": [{ "name": "web", "port": 80, "targetPort": "http", "nodePort": 31080, "protocol": "TCP" }] },
              "status": { "loadBalancer": { "ingress": [{ "hostname": "k8s-shop-api-123.elb.us-east-1.amazonaws.com" }] } } },
            { "kind": "Service", "metadata": { "name": "db", "namespace": "shop" }, "spec": { "clusterIP": "None", "selector": { "app": "db" }, "ports": [{ "port": 5432, "targetPort": 5432 }] } },
            { "kind": "Service", "metadata": { "name": "external", "namespace": "shop" }, "spec": { "type": "ExternalName", "ports": [{ "port": 443 }] } },
            { "kind": "Ingress", "metadata": { "name": "shop", "namespace": "shop", "annotations": { "kubernetes.io/ingress.class": "nginx" } },
              "spec": { "tls": [{ "hosts": ["shop.example.com"] }], "rules": [{ "host": "shop.example.com", "http": { "paths": [{ "path": "/api", "backend": { "service": { "name": "api", "port": { "name": "web" } } } }, { "path": "/db", "backend": { "service": { "name": "db", "port": { "number": 5432 } } } }] } }] },
              "status": { "loadBalancer": { "ingress": [{ "ip": "203.0.113.10" }] } } }
        ] })
    }

    #[test]
    fn a_cluster_becomes_workloads_services_and_routes() {
        let inv = parse(&sample());
        let item = |id: &str| inv.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id}"));
        let node = item("node:node-a");
        assert_eq!((node.ips.clone(), node.state.as_str(), node.detail.as_str(), node.scope.as_str()), (vec!["10.0.1.5".to_string()], "Ready", "v1.30.2 · t3.large", "us-east-1a"));
        assert!(node.refs.contains(&"i-0abc".to_string()) && node.refs.contains(&"ip-10-0-1-5.ec2.internal".to_string()), "known by its instance id and host name");
        let api = item("wl:shop/Deployment/api");
        assert_eq!((api.name.as_str(), api.scope.as_str(), api.ips.clone(), api.state.as_str(), api.detail.as_str(), api.ports.clone()), ("api", "shop", vec!["10.244.1.7".to_string()], "1 of 2 running", "acme/api:2.1", vec![(3000, "tcp".to_string(), "http".to_string())]));
        assert_eq!(api.attrs, [("Kind".to_string(), "Deployment".to_string()), ("Pods".to_string(), "2".to_string()), ("Nodes".to_string(), "node-a, node-b".to_string())]);
        assert!(item("wl:kube-system/DaemonSet/kube-proxy").ips.is_empty(), "a host-network pod has no address of its own");
        assert_eq!(item("wl:shop/CronJob/backup").state, "0 of 1 running");
        let svc = item("svc:shop/api");
        assert_eq!((svc.detail.as_str(), svc.ips.clone(), svc.ports.clone(), svc.node_ports.clone(), svc.public), ("LoadBalancer", vec!["10.96.0.20".to_string()], vec![(80, "tcp".to_string(), "web · node port 31080".to_string())], vec![31080], true));
        assert!(item("svc:shop/db").ips.is_empty(), "headless");
        let ing = item("ing:shop/shop");
        assert_eq!((ing.detail.as_str(), ing.ports[0].0, ing.attrs.clone(), ing.refs.is_empty()), ("shop.example.com", 443, vec![("Class".to_string(), "nginx".to_string()), ("Load balancer".to_string(), "203.0.113.10".to_string())], true));
        let links: Vec<String> = inv.links.iter().map(|l| format!("{} -> {} :{} {}", l.from, l.to, l.port, l.note)).collect();
        assert_eq!(
            links,
            [
                "id:svc:shop/api -> id:wl:shop/Deployment/api :3000 service port 80 → 3000", // the named target port, resolved
                "ext:k8s-shop-api-123.elb.us-east-1.amazonaws.com -> id:svc:shop/api :80 load balancer of the service",
                "id:svc:shop/db -> id:wl:shop/StatefulSet/db :5432 ",
                "id:ing:shop/shop -> id:svc:shop/api :80 shop.example.com/api", // the named service port, resolved
                "id:ing:shop/shop -> id:svc:shop/db :5432 shop.example.com/db",
                "ext:203.0.113.10 -> id:ing:shop/shop :443 load balancer of the ingress",
            ]
        );
    }
}
