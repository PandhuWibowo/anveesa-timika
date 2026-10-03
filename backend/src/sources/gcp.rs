//! Google Cloud: Compute Engine instances, load balancers (forwarding rules,
//! through their proxies and URL maps to backend services and the instances of
//! their managed groups) and Cloud NAT. Through `gcloud` on a server, or the
//! Compute API with a service-account key from the vault.

use std::collections::HashMap;

use serde_json::Value;

use super::fetch::Via;
use super::{list, num, push, s, tail, Inventory, Item, Source};
use crate::error::AppResult;

/// (name in the API, `gcloud compute <words> list`)
const LISTS: [(&str, &str); 8] = [
    ("instances", "instances"),
    ("forwardingRules", "forwarding-rules"),
    ("backendServices", "backend-services"),
    ("urlMaps", "url-maps"),
    ("targetHttpProxies", "target-http-proxies"),
    ("targetHttpsProxies", "target-https-proxies"),
    ("routers", "routers"),
    ("addresses", "addresses"),
];

/// `gcloud … list` prints a list; the API's aggregated list groups by zone or
/// region: { "zones/us-central1-a": { "instances": […] } }. Either way: the list.
fn flatten(v: &Value, key: &str) -> Vec<Value> {
    if let Value::Array(a) = v {
        return a.clone();
    }
    v["items"].as_object().into_iter().flat_map(|scopes| scopes.values()).flat_map(|scope| list(scope, key).into_iter().cloned().collect::<Vec<_>>()).collect()
}

pub async fn collect(via: &Via, src: &Source) -> AppResult<Inventory> {
    let mut data: HashMap<&str, Vec<Value>> = HashMap::new();
    let mut notes = Vec::new();
    for (i, (name, words)) in LISTS.iter().enumerate() {
        let got: AppResult<Vec<Value>> = async {
            if via.is_cli() {
                return Ok(flatten(&via.cli("gcloud", &["compute", words, "list", "--project", &src.scope, "--format=json", "--quiet"]).await?, name));
            }
            let token = via.google_token(&src.endpoint).await?;
            let base = if src.endpoint.is_empty() { "https://compute.googleapis.com" } else { &src.endpoint };
            let mut all = Vec::new();
            let mut page = String::new();
            for _ in 0..5 {
                let url = format!("{base}/compute/v1/projects/{}/aggregated/{name}?maxResults=500{}", src.scope, if page.is_empty() { String::new() } else { format!("&pageToken={}", super::fetch::urlencode(&page)) });
                let v = via.get_json(&format!("google {name}"), &url, &token).await?;
                all.extend(flatten(&v, name));
                page = s(&v, "nextPageToken");
                if page.is_empty() {
                    break;
                }
            }
            Ok(all)
        }
        .await;
        match got {
            Ok(v) => {
                data.insert(name, v);
            }
            // Without instances there is nothing to show; the rest is detail.
            Err(e) if i == 0 => return Err(e),
            Err(e) => notes.push(format!("{name}: {e}")),
        }
    }
    let mut inv = parse(&data);
    inv.notes = notes;
    Ok(inv)
}

pub fn parse(d: &HashMap<&str, Vec<Value>>) -> Inventory {
    let mut inv = Inventory::default();
    let of = |k: &str| d.get(k).map(Vec::as_slice).unwrap_or_default();
    // address resource → the address
    let addr: HashMap<String, String> = of("addresses").iter().map(|a| (s(a, "selfLink"), s(a, "address"))).collect();

    // managed instance group → its instances; (network, region) → instances without a public address
    let mut in_group: HashMap<String, Vec<String>> = HashMap::new();
    let mut private: HashMap<(String, String), Vec<String>> = HashMap::new();
    for i in of("instances") {
        let name = s(i, "name");
        let zone = tail(&s(i, "zone")).to_string();
        let id = format!("vm:{zone}/{name}");
        let mut item = Item { id: id.clone(), kind: "vm".into(), name: name.clone(), scope: zone.clone(), detail: tail(&s(i, "machineType")).to_string(), state: s(i, "status").to_lowercase(), refs: vec![name.clone(), s(i, "id"), s(i, "selfLink")], ..Default::default() };
        let region = zone.rsplit_once('-').map(|x| x.0.to_string()).unwrap_or_default();
        for nic in list(i, "networkInterfaces") {
            push(&mut item.ips, s(nic, "networkIP"));
            let public: Vec<String> = list(nic, "accessConfigs").into_iter().map(|a| s(a, "natIP")).filter(|a| !a.is_empty()).collect();
            if public.is_empty() {
                private.entry((s(nic, "network"), region.clone())).or_default().push(id.clone());
            }
            item.public |= !public.is_empty();
            public.into_iter().for_each(|a| push(&mut item.ips, a));
            item.attrs.push(("Network".into(), format!("{} / {}", tail(&s(nic, "network")), tail(&s(nic, "subnetwork")))));
        }
        // "created-by": ".../instanceGroupManagers/web-mig"
        if let Some(by) = list(&i["metadata"], "items").into_iter().find(|m| s(m, "key") == "created-by") {
            in_group.entry(tail(&s(by, "value")).to_string()).or_default().push(id);
        }
        item.refs.retain(|r| !r.is_empty());
        inv.items.push(item);
    }

    // backend service → (port name, the instances behind it)
    let services: HashMap<String, &Value> = of("backendServices").iter().map(|b| (s(b, "selfLink"), b)).collect();
    let members = |svc: &str| -> Vec<String> { services.get(svc).into_iter().flat_map(|b| list(b, "backends")).flat_map(|b| in_group.get(tail(&s(b, "group"))).cloned().unwrap_or_default()).collect() };
    // URL map → every backend service it can send to
    let maps: HashMap<String, Vec<String>> = of("urlMaps")
        .iter()
        .map(|m| {
            let mut out = vec![s(m, "defaultService")];
            for pm in list(m, "pathMatchers") {
                push(&mut out, s(pm, "defaultService"));
                for rule in list(pm, "pathRules") {
                    push(&mut out, s(rule, "service"));
                }
            }
            out.retain(|x| !x.is_empty());
            (s(m, "selfLink"), out)
        })
        .collect();
    let proxies: HashMap<String, String> = of("targetHttpProxies").iter().chain(of("targetHttpsProxies")).map(|p| (s(p, "selfLink"), s(p, "urlMap"))).collect();

    for f in of("forwardingRules") {
        let name = s(f, "name");
        let id = format!("lb:{name}");
        let scheme = s(f, "loadBalancingScheme");
        let public = scheme.starts_with("EXTERNAL");
        let mut item = Item { id: id.clone(), kind: "lb".into(), name: name.clone(), scope: tail(&s(f, "region")).to_string(), detail: format!("load balancer · {}", scheme.to_lowercase().replace('_', " ")), refs: vec![name, s(f, "selfLink")], public, ..Default::default() };
        if item.scope.is_empty() {
            item.scope = "global".into();
        }
        push(&mut item.ips, s(f, "IPAddress"));
        // "443-443", or a list of ports.
        let mut ports: Vec<u16> = list(f, "ports").into_iter().filter_map(|p| p.as_str().and_then(|x| x.parse().ok())).collect();
        if let Some(p) = s(f, "portRange").split('-').next().and_then(|x| x.parse().ok()) {
            ports.push(p);
        }
        ports.dedup();
        for p in &ports {
            item.ports.push((*p, s(f, "IPProtocol").to_lowercase(), String::new()));
            if public {
                inv.link("internet", format!("id:{id}"), *p, "");
            }
        }
        // Straight to a backend service (network / internal), or through a proxy and its URL map.
        let mut backends = vec![s(f, "backendService")];
        if let Some(map) = proxies.get(&s(f, "target")) {
            backends.extend(maps.get(map).cloned().unwrap_or_default());
        }
        for b in backends.iter().filter(|b| !b.is_empty()) {
            let svc = services.get(b);
            // The group's named port is not in these lists; the service's own port is the best known.
            let port = svc.map(|x| num(x, "port")).filter(|p| *p > 0).or(ports.first().copied()).unwrap_or(0);
            for vm in members(b) {
                inv.link(format!("id:{id}"), format!("id:{vm}"), port, format!("backend service {}", tail(b)));
            }
        }
        inv.items.push(item);
    }

    for r in of("routers") {
        let region = tail(&s(r, "region")).to_string();
        for n in list(r, "nats") {
            let name = s(n, "name");
            let id = format!("nat:{region}/{name}");
            let mut item = Item { id: id.clone(), kind: "nat".into(), name, scope: region.clone(), detail: "Cloud NAT".into(), state: "running".into(), ..Default::default() };
            for ip in list(n, "natIps") {
                let link = ip.as_str().unwrap_or_default();
                push(&mut item.ips, addr.get(link).cloned().unwrap_or_default());
            }
            item.attrs.push(("Router".into(), s(r, "name")));
            item.attrs.push(("Network".into(), tail(&s(r, "network")).to_string()));
            inv.link(format!("id:{id}"), "internet", 0, "outbound");
            // Cloud NAT serves the instances of its network and region that have no public address.
            for vm in private.get(&(s(r, "network"), region.clone())).into_iter().flatten() {
                inv.link(format!("id:{vm}"), format!("id:{id}"), 0, "internet access (no public address)");
            }
            inv.items.push(item);
        }
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_shapes_of_a_list() {
        let api = json!({ "items": { "zones/a": { "instances": [{ "name": "x" }] }, "zones/b": { "warning": { "code": "NO_RESULTS_ON_PAGE" } }, "zones/c": { "instances": [{ "name": "y" }] } } });
        assert_eq!(flatten(&api, "instances").len(), 2);
        assert_eq!(flatten(&json!([{ "name": "x" }]), "instances").len(), 1);
    }

    #[test]
    fn instances_load_balancers_and_cloud_nat() {
        let p = "https://www.googleapis.com/compute/v1/projects/acme";
        let d: HashMap<&str, Vec<Value>> = HashMap::from([
            ("instances", vec![
                json!({ "name": "web-abcd", "id": "111", "zone": format!("{p}/zones/us-central1-a"), "machineType": format!("{p}/zones/us-central1-a/machineTypes/e2-medium"), "status": "RUNNING", "selfLink": format!("{p}/zones/us-central1-a/instances/web-abcd"),
                    "networkInterfaces": [{ "networkIP": "10.128.0.5", "network": format!("{p}/global/networks/prod"), "subnetwork": format!("{p}/regions/us-central1/subnetworks/prod") }],
                    "metadata": { "items": [{ "key": "created-by", "value": "projects/1/zones/us-central1-a/instanceGroupManagers/web-mig" }] } }),
                json!({ "name": "bastion", "id": "222", "zone": format!("{p}/zones/us-central1-b"), "machineType": format!("{p}/zones/us-central1-b/machineTypes/e2-small"), "status": "RUNNING",
                    "networkInterfaces": [{ "networkIP": "10.128.0.9", "network": format!("{p}/global/networks/prod"), "accessConfigs": [{ "natIP": "34.1.2.3" }] }] }),
            ]),
            ("forwardingRules", vec![json!({ "name": "web-https", "selfLink": format!("{p}/global/forwardingRules/web-https"), "IPAddress": "34.120.0.1", "IPProtocol": "TCP", "portRange": "443-443", "loadBalancingScheme": "EXTERNAL_MANAGED", "target": format!("{p}/global/targetHttpsProxies/web") })]),
            ("targetHttpsProxies", vec![json!({ "selfLink": format!("{p}/global/targetHttpsProxies/web"), "urlMap": format!("{p}/global/urlMaps/web") })]),
            ("urlMaps", vec![json!({ "selfLink": format!("{p}/global/urlMaps/web"), "defaultService": format!("{p}/global/backendServices/web-bs") })]),
            ("backendServices", vec![json!({ "selfLink": format!("{p}/global/backendServices/web-bs"), "port": 8080, "backends": [{ "group": format!("{p}/zones/us-central1-a/instanceGroups/web-mig") }] })]),
            ("routers", vec![json!({ "name": "nat-router", "region": format!("{p}/regions/us-central1"), "network": format!("{p}/global/networks/prod"), "nats": [{ "name": "egress", "natIps": [format!("{p}/regions/us-central1/addresses/nat-ip")] }] })]),
            ("addresses", vec![json!({ "selfLink": format!("{p}/regions/us-central1/addresses/nat-ip"), "address": "35.9.9.9" })]),
        ]);
        let inv = parse(&d);
        let item = |id: &str| inv.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id}"));
        let web = item("vm:us-central1-a/web-abcd");
        assert_eq!((web.ips.clone(), web.public, web.detail.as_str(), web.state.as_str(), web.attrs.clone()), (vec!["10.128.0.5".to_string()], false, "e2-medium", "running", vec![("Network".to_string(), "prod / prod".to_string())]));
        assert_eq!((item("vm:us-central1-b/bastion").ips.clone(), item("vm:us-central1-b/bastion").public), (vec!["10.128.0.9".to_string(), "34.1.2.3".to_string()], true));
        let lb = item("lb:web-https");
        assert_eq!((lb.ips.clone(), lb.public, lb.scope.as_str(), lb.ports.iter().map(|p| p.0).collect::<Vec<_>>(), lb.detail.as_str()), (vec!["34.120.0.1".to_string()], true, "global", vec![443], "load balancer · external managed"));
        assert_eq!(item("nat:us-central1/egress").ips, ["35.9.9.9"]);
        let links: Vec<String> = inv.links.iter().map(|l| format!("{} -> {} :{} {}", l.from, l.to, l.port, l.note)).collect();
        assert_eq!(
            links,
            [
                "internet -> id:lb:web-https :443 ",
                "id:lb:web-https -> id:vm:us-central1-a/web-abcd :8080 backend service web-bs", // rule → proxy → URL map → service → group → instance
                "id:nat:us-central1/egress -> internet :0 outbound",
                "id:vm:us-central1-a/web-abcd -> id:nat:us-central1/egress :0 internet access (no public address)", // not the bastion: it has its own address
            ]
        );
    }
}
