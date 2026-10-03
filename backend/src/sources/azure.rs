//! Azure: virtual machines (with the addresses of their network interfaces),
//! load balancers and application gateways with their backends, and NAT
//! gateways with the subnets attached to them. Through `az rest` on a server,
//! or Resource Manager with an app registration from the vault — the same
//! documents either way.

use std::collections::HashMap;

use serde_json::Value;

use super::fetch::Via;
use super::{list, num, push, s, tail, Inventory, Item, Source};
use crate::error::AppResult;

/// (name here, resource type, api-version)
const LISTS: [(&str, &str, &str); 6] = [
    ("vms", "Microsoft.Compute/virtualMachines", "2024-03-01"),
    ("nics", "Microsoft.Network/networkInterfaces", "2023-09-01"),
    ("pips", "Microsoft.Network/publicIPAddresses", "2023-09-01"),
    ("lbs", "Microsoft.Network/loadBalancers", "2023-09-01"),
    ("nats", "Microsoft.Network/natGateways", "2023-09-01"),
    ("appgws", "Microsoft.Network/applicationGateways", "2023-09-01"),
];

pub async fn collect(via: &Via, src: &Source) -> AppResult<Inventory> {
    let arm = if src.endpoint.is_empty() { "https://management.azure.com" } else { &src.endpoint };
    let mut data: HashMap<&str, Vec<Value>> = HashMap::new();
    let mut notes = Vec::new();
    for (i, (name, kind, version)) in LISTS.iter().enumerate() {
        let got: AppResult<Vec<Value>> = async {
            let mut url = format!("{arm}/subscriptions/{}/providers/{kind}?api-version={version}", src.scope);
            let mut all = Vec::new();
            for _ in 0..5 {
                let v = if via.is_cli() { via.cli("az", &["rest", "--method", "get", "--url", &url, "--output", "json"]).await? } else { via.get_json(&format!("azure {name}"), &url, &via.azure_token(&src.endpoint).await?).await? };
                all.extend(list(&v, "value").into_iter().cloned());
                url = s(&v, "nextLink");
                // Only the provider's own next page is followed.
                if url.is_empty() || !url.starts_with(arm) {
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
            Err(e) if i == 0 => return Err(e),
            Err(e) => notes.push(format!("{name}: {e}")),
        }
    }
    let mut inv = parse(&data);
    inv.notes = notes;
    Ok(inv)
}

/// Resource ids compare without case.
fn key(id: &str) -> String {
    id.to_lowercase()
}
/// ".../resourceGroups/prod/providers/…/name" → "prod/name"
fn short(id: &str) -> String {
    let rg = id.split('/').collect::<Vec<_>>().windows(2).find(|w| w[0].eq_ignore_ascii_case("resourceGroups")).map(|w| w[1].to_string()).unwrap_or_default();
    format!("{rg}/{}", tail(id))
}

pub fn parse(d: &HashMap<&str, Vec<Value>>) -> Inventory {
    let mut inv = Inventory::default();
    let of = |k: &str| d.get(k).map(Vec::as_slice).unwrap_or_default();
    let pip: HashMap<String, String> = of("pips").iter().map(|p| (key(&s(p, "id")), s(&p["properties"], "ipAddress"))).collect();
    let public_ip = |v: &Value| pip.get(&key(&s(&v["publicIPAddress"], "id"))).cloned().unwrap_or_default();

    // What each VM's interfaces say: addresses, subnets, and the backend pools it is in.
    struct Nic {
        ips: Vec<String>,
        public: bool,
        subnets: Vec<String>,
        pools: Vec<String>,
    }
    let mut nics: HashMap<String, Nic> = HashMap::new();
    for n in of("nics") {
        let vm = key(&s(&n["properties"]["virtualMachine"], "id"));
        if vm.is_empty() {
            continue;
        }
        let e = nics.entry(vm).or_insert(Nic { ips: vec![], public: false, subnets: vec![], pools: vec![] });
        for c in list(&n["properties"], "ipConfigurations") {
            let p = &c["properties"];
            push(&mut e.ips, s(p, "privateIPAddress"));
            let public = public_ip(p);
            e.public |= !public.is_empty();
            push(&mut e.ips, public);
            push(&mut e.subnets, key(&s(&p["subnet"], "id")));
            for pool in list(p, "loadBalancerBackendAddressPools").into_iter().chain(list(p, "applicationGatewayBackendAddressPools")) {
                push(&mut e.pools, key(&s(pool, "id")));
            }
        }
    }
    for v in of("vms") {
        let id = s(v, "id");
        let mut item = Item { id: format!("vm:{}", short(&id)), kind: "vm".into(), name: s(v, "name"), scope: [s(v, "location"), list(v, "zones").first().and_then(|z| z.as_str()).unwrap_or_default().to_string()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-"), detail: s(&v["properties"]["hardwareProfile"], "vmSize"), state: s(&v["properties"], "provisioningState").to_lowercase(), refs: vec![key(&id)], ..Default::default() };
        if let Some(n) = nics.get(&key(&id)) {
            item.ips = n.ips.clone();
            item.public = n.public;
            item.attrs.push(("Subnet".into(), n.subnets.iter().map(|x| tail(x).to_string()).collect::<Vec<_>>().join(", ")));
        }
        item.attrs.insert(0, ("Resource group".into(), short(&id).split('/').next().unwrap_or_default().to_string()));
        inv.items.push(item);
    }
    let in_pool = |pool: &str| -> Vec<String> { nics.iter().filter(|(_, n)| n.pools.iter().any(|p| p == pool)).map(|(vm, _)| format!("ref:{vm}")).collect() };

    for lb in of("lbs") {
        let p = &lb["properties"];
        let id = format!("lb:{}", short(&s(lb, "id")));
        let mut item = Item { id: id.clone(), kind: "lb".into(), name: s(lb, "name"), scope: s(lb, "location"), detail: format!("Load Balancer · {}", s(&lb["sku"], "name")), state: s(p, "provisioningState").to_lowercase(), refs: vec![key(&s(lb, "id"))], ..Default::default() };
        for f in list(p, "frontendIPConfigurations") {
            push(&mut item.ips, s(&f["properties"], "privateIPAddress"));
            let public = public_ip(&f["properties"]);
            item.public |= !public.is_empty();
            push(&mut item.ips, public);
        }
        // Pools may also list plain addresses.
        let direct: HashMap<String, Vec<String>> = list(p, "backendAddressPools").into_iter().map(|pool| (key(&s(pool, "id")), list(&pool["properties"], "loadBalancerBackendAddresses").into_iter().map(|a| s(&a["properties"], "ipAddress")).filter(|a| !a.is_empty()).map(|a| format!("ip:{a}")).collect())).collect();
        for r in list(p, "loadBalancingRules") {
            let rp = &r["properties"];
            let (front, back) = (num(rp, "frontendPort"), num(rp, "backendPort"));
            item.ports.push((front, s(rp, "protocol").to_lowercase(), s(r, "name")));
            if item.public {
                inv.link("internet", format!("id:{id}"), front, "");
            }
            let pool = key(&s(&rp["backendAddressPool"], "id"));
            let mut targets = in_pool(&pool);
            targets.extend(direct.get(&pool).cloned().unwrap_or_default());
            targets.sort();
            for t in targets {
                inv.link(format!("id:{id}"), t, back, format!("rule {} · {front} → {back}", s(r, "name")));
            }
        }
        inv.items.push(item);
    }

    for g in of("appgws") {
        let p = &g["properties"];
        let id = format!("lb:{}", short(&s(g, "id")));
        let mut item = Item { id: id.clone(), kind: "lb".into(), name: s(g, "name"), scope: s(g, "location"), detail: "Application Gateway".into(), state: s(p, "provisioningState").to_lowercase(), refs: vec![key(&s(g, "id"))], ..Default::default() };
        for f in list(p, "frontendIPConfigurations") {
            push(&mut item.ips, s(&f["properties"], "privateIPAddress"));
            let public = public_ip(&f["properties"]);
            item.public |= !public.is_empty();
            push(&mut item.ips, public);
        }
        for f in list(p, "frontendPorts") {
            let port = num(&f["properties"], "port");
            item.ports.push((port, "http".into(), String::new()));
            if item.public {
                inv.link("internet", format!("id:{id}"), port, "");
            }
        }
        let back = list(p, "backendHttpSettingsCollection").first().map(|x| num(&x["properties"], "port")).unwrap_or(0);
        for pool in list(p, "backendAddressPools") {
            let mut targets = in_pool(&key(&s(pool, "id")));
            for a in list(&pool["properties"], "backendAddresses") {
                match (s(a, "ipAddress"), s(a, "fqdn")) {
                    (ip, _) if !ip.is_empty() => targets.push(format!("ip:{ip}")),
                    (_, name) if !name.is_empty() => targets.push(format!("ip:{name}")),
                    _ => {}
                }
            }
            targets.sort();
            for t in targets {
                inv.link(format!("id:{id}"), t, back, format!("backend pool {}", s(pool, "name")));
            }
        }
        inv.items.push(item);
    }

    for n in of("nats") {
        let p = &n["properties"];
        let id = format!("nat:{}", short(&s(n, "id")));
        let mut item = Item { id: id.clone(), kind: "nat".into(), name: s(n, "name"), scope: s(n, "location"), detail: "NAT gateway".into(), state: s(p, "provisioningState").to_lowercase(), refs: vec![key(&s(n, "id"))], ..Default::default() };
        for a in list(p, "publicIpAddresses") {
            push(&mut item.ips, pip.get(&key(&s(a, "id"))).cloned().unwrap_or_default());
        }
        inv.link(format!("id:{id}"), "internet", 0, "outbound");
        let subnets: Vec<String> = list(p, "subnets").into_iter().map(|x| key(&s(x, "id"))).collect();
        let mut vms: Vec<(&String, &Nic)> = nics.iter().filter(|(_, nic)| nic.subnets.iter().any(|x| subnets.contains(x))).collect();
        vms.sort_by_key(|x| x.0);
        for (vm, nic) in vms {
            let subnet = nic.subnets.iter().find(|x| subnets.contains(x)).map(|x| tail(x)).unwrap_or_default();
            inv.link(format!("ref:{vm}"), format!("id:{id}"), 0, format!("internet access of {subnet}"));
        }
        inv.items.push(item);
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vms_load_balancer_gateway_and_nat() {
        let sub = "/subscriptions/s1/resourceGroups/Prod/providers";
        let d: HashMap<&str, Vec<Value>> = HashMap::from([
            ("vms", vec![
                json!({ "id": format!("{sub}/Microsoft.Compute/virtualMachines/web-1"), "name": "web-1", "location": "southeastasia", "zones": ["1"], "properties": { "provisioningState": "Succeeded", "hardwareProfile": { "vmSize": "Standard_B2s" } } }),
                json!({ "id": format!("{sub}/Microsoft.Compute/virtualMachines/jump"), "name": "jump", "location": "southeastasia", "properties": { "provisioningState": "Succeeded", "hardwareProfile": { "vmSize": "Standard_B1s" } } }),
            ]),
            ("nics", vec![
                json!({ "properties": { "virtualMachine": { "id": format!("{sub}/Microsoft.Compute/virtualMachines/WEB-1") }, "ipConfigurations": [{ "properties": { "privateIPAddress": "10.1.0.4", "subnet": { "id": format!("{sub}/Microsoft.Network/virtualNetworks/prod/subnets/app") }, "loadBalancerBackendAddressPools": [{ "id": format!("{sub}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web") }] } }] } }),
                json!({ "properties": { "virtualMachine": { "id": format!("{sub}/Microsoft.Compute/virtualMachines/jump") }, "ipConfigurations": [{ "properties": { "privateIPAddress": "10.1.1.4", "publicIPAddress": { "id": format!("{sub}/Microsoft.Network/publicIPAddresses/jump-ip") }, "subnet": { "id": format!("{sub}/Microsoft.Network/virtualNetworks/prod/subnets/mgmt") } } }] } }),
                json!({ "properties": { "ipConfigurations": [{ "properties": { "privateIPAddress": "10.1.9.9" } }] } }),
            ]),
            ("pips", vec![
                json!({ "id": format!("{sub}/Microsoft.Network/publicIPAddresses/jump-ip"), "properties": { "ipAddress": "20.1.2.3" } }),
                json!({ "id": format!("{sub}/Microsoft.Network/publicIPAddresses/shop-ip"), "properties": { "ipAddress": "20.9.9.9" } }),
                json!({ "id": format!("{sub}/Microsoft.Network/publicIPAddresses/nat-ip"), "properties": { "ipAddress": "20.5.5.5" } }),
            ]),
            ("lbs", vec![json!({ "id": format!("{sub}/Microsoft.Network/loadBalancers/shop"), "name": "shop", "location": "southeastasia", "sku": { "name": "Standard" }, "properties": { "provisioningState": "Succeeded",
                "frontendIPConfigurations": [{ "properties": { "publicIPAddress": { "id": format!("{sub}/Microsoft.Network/publicIPAddresses/SHOP-IP") } } }],
                "backendAddressPools": [{ "id": format!("{sub}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web"), "properties": { "loadBalancerBackendAddresses": [{ "properties": { "ipAddress": "10.1.0.77" } }] } }],
                "loadBalancingRules": [{ "name": "https", "properties": { "frontendPort": 443, "backendPort": 8443, "protocol": "Tcp", "backendAddressPool": { "id": format!("{sub}/Microsoft.Network/loadBalancers/shop/backendAddressPools/web") } } }] } })]),
            ("nats", vec![json!({ "id": format!("{sub}/Microsoft.Network/natGateways/egress"), "name": "egress", "location": "southeastasia", "properties": { "provisioningState": "Succeeded", "publicIpAddresses": [{ "id": format!("{sub}/Microsoft.Network/publicIPAddresses/nat-ip") }], "subnets": [{ "id": format!("{sub}/Microsoft.Network/virtualNetworks/prod/subnets/app") }] } })]),
            ("appgws", vec![json!({ "id": format!("{sub}/Microsoft.Network/applicationGateways/waf"), "name": "waf", "location": "southeastasia", "properties": { "provisioningState": "Succeeded",
                "frontendIPConfigurations": [{ "properties": { "privateIPAddress": "10.1.5.10" } }], "frontendPorts": [{ "properties": { "port": 443 } }],
                "backendHttpSettingsCollection": [{ "properties": { "port": 8080 } }], "backendAddressPools": [{ "name": "api", "properties": { "backendAddresses": [{ "ipAddress": "10.1.0.4" }, { "fqdn": "api.internal.example.com" }] } }] } })]),
        ]);
        let inv = parse(&d);
        let item = |id: &str| inv.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id}"));
        let web = item("vm:Prod/web-1");
        assert_eq!((web.ips.clone(), web.public, web.scope.as_str(), web.detail.as_str(), web.state.as_str()), (vec!["10.1.0.4".to_string()], false, "southeastasia-1", "Standard_B2s", "succeeded"));
        assert_eq!(web.attrs, [("Resource group".to_string(), "Prod".to_string()), ("Subnet".to_string(), "app".to_string())]);
        assert_eq!((item("vm:Prod/jump").ips.clone(), item("vm:Prod/jump").public), (vec!["10.1.1.4".to_string(), "20.1.2.3".to_string()], true));
        let lb = item("lb:Prod/shop");
        assert_eq!((lb.ips.clone(), lb.public, lb.ports.clone(), lb.detail.as_str()), (vec!["20.9.9.9".to_string()], true, vec![(443, "tcp".to_string(), "https".to_string())], "Load Balancer · Standard"));
        assert_eq!((item("lb:Prod/waf").ips.clone(), item("lb:Prod/waf").public), (vec!["10.1.5.10".to_string()], false));
        assert_eq!(item("nat:Prod/egress").ips, ["20.5.5.5"]);
        let vm = "/subscriptions/s1/resourcegroups/prod/providers/microsoft.compute/virtualmachines/web-1";
        let links: Vec<String> = inv.links.iter().map(|l| format!("{} -> {} :{} {}", l.from, l.to, l.port, l.note)).collect();
        assert_eq!(
            links,
            [
                "internet -> id:lb:Prod/shop :443 ".to_string(),
                "id:lb:Prod/shop -> ip:10.1.0.77 :8443 rule https · 443 → 8443".into(),
                format!("id:lb:Prod/shop -> ref:{vm} :8443 rule https · 443 → 8443"), // through its interface's pool membership, whatever the id's case
                "id:lb:Prod/waf -> ip:10.1.0.4 :8080 backend pool api".into(),
                "id:lb:Prod/waf -> ip:api.internal.example.com :8080 backend pool api".into(),
                "id:nat:Prod/egress -> internet :0 outbound".into(),
                format!("ref:{vm} -> id:nat:Prod/egress :0 internet access of app"),
            ]
        );
    }
}
