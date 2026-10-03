//! Tencent Cloud: CVM instances, CLB load balancers with their listeners and
//! targets, NAT gateways and the subnets routed through them. Through `tccli`
//! on a server, or API 3.0 signed with a key from the vault.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::fetch::Via;
use super::{list, num, push, s, Inventory, Item, Source};
use crate::error::AppResult;

#[derive(Default)]
pub struct Region {
    pub instances: Value,
    pub lbs: Value,
    /// (load balancer id, its listeners with targets)
    pub targets: Vec<(String, Value)>,
    pub nats: Value,
    pub routes: Value,
}

/// One Describe* action, all pages of its `set`.
async fn call(via: &Via, src: &Source, region: &str, service: &str, version: &str, action: &str, set: &str, extra: &[(&str, &str)]) -> AppResult<Value> {
    let mut all = Vec::new();
    let mut out = json!({});
    for page in 0..if set.is_empty() { 1 } else { 10 } {
        let offset = (page * 100).to_string();
        let v = if via.is_cli() {
            let mut args: Vec<&str> = vec![service, action, "--region", region, "--output", "json"];
            for (k, val) in extra {
                args.push(k);
                args.push(val);
            }
            if !set.is_empty() {
                args.extend(["--Limit", "100", "--Offset", &offset]);
            }
            let v = via.cli("tccli", &args).await?;
            v.get("Response").cloned().unwrap_or(v)
        } else {
            let mut payload = serde_json::Map::new();
            for (k, val) in extra {
                payload.insert(k.trim_start_matches("--").to_string(), json!(val));
            }
            if !set.is_empty() {
                // DescribeRouteTables alone takes these as strings.
                let text = action == "DescribeRouteTables";
                payload.insert("Limit".into(), if text { json!("100") } else { json!(100) });
                payload.insert("Offset".into(), if text { json!(offset) } else { json!(page * 100) });
            }
            via.tencent(&src.endpoint, service, version, region, action, Value::Object(payload)).await?
        };
        let got = list(&v, set).len();
        all.extend(list(&v, set).into_iter().cloned());
        out = v;
        if set.is_empty() || got < 100 {
            break;
        }
    }
    if !set.is_empty() {
        out[set] = Value::Array(all);
    }
    Ok(out)
}

async fn region(via: &Via, src: &Source, r: &str) -> AppResult<Region> {
    let mut d = Region {
        instances: call(via, src, r, "cvm", "2017-03-12", "DescribeInstances", "InstanceSet", &[]).await?,
        lbs: call(via, src, r, "clb", "2018-03-17", "DescribeLoadBalancers", "LoadBalancerSet", &[]).await?,
        nats: call(via, src, r, "vpc", "2017-03-12", "DescribeNatGateways", "NatGatewaySet", &[]).await?,
        routes: call(via, src, r, "vpc", "2017-03-12", "DescribeRouteTables", "RouteTableSet", &[]).await?,
        ..Default::default()
    };
    for lb in list(&d.lbs, "LoadBalancerSet").into_iter().take(40) {
        let id = s(lb, "LoadBalancerId");
        if let Ok(v) = call(via, src, r, "clb", "2018-03-17", "DescribeTargets", "", &[("--LoadBalancerId", &id)]).await {
            d.targets.push((id, v));
        }
    }
    Ok(d)
}

pub async fn collect(via: &Via, src: &Source) -> AppResult<Inventory> {
    let mut inv = Inventory::default();
    let mut failed = None;
    for r in &src.regions {
        match region(via, src, r).await {
            Ok(d) => inv.extend(parse(r, &d)),
            Err(e) => {
                inv.notes.push(format!("{r}: {e}"));
                failed.get_or_insert(e);
            }
        }
    }
    match failed {
        Some(e) if inv.notes.len() == src.regions.len() => Err(e),
        _ => Ok(inv),
    }
}

fn strings(v: &Value, k: &str) -> Vec<String> {
    list(v, k).into_iter().filter_map(|x| x.as_str().map(String::from)).collect()
}

pub fn parse(region: &str, d: &Region) -> Inventory {
    let mut inv = Inventory::default();
    let mut by_subnet: HashMap<String, Vec<String>> = HashMap::new();
    for i in list(&d.instances, "InstanceSet") {
        let id = s(i, "InstanceId");
        if id.is_empty() {
            continue;
        }
        let name = s(i, "InstanceName");
        let mut item = Item { id: format!("vm:{id}"), kind: "vm".into(), name: if name.is_empty() { id.clone() } else { name }, scope: s(&i["Placement"], "Zone"), detail: s(i, "InstanceType"), state: s(i, "InstanceState").to_lowercase(), refs: vec![id.clone()], ..Default::default() };
        strings(i, "PrivateIpAddresses").into_iter().for_each(|a| push(&mut item.ips, a));
        let public = strings(i, "PublicIpAddresses");
        item.public = !public.is_empty();
        public.into_iter().for_each(|a| push(&mut item.ips, a));
        let net = &i["VirtualPrivateCloud"];
        item.attrs = vec![("Instance".into(), id), ("VPC".into(), s(net, "VpcId")), ("Subnet".into(), s(net, "SubnetId"))];
        by_subnet.entry(s(net, "SubnetId")).or_default().push(item.id.clone());
        inv.items.push(item);
    }

    for lb in list(&d.lbs, "LoadBalancerSet") {
        let lid = s(lb, "LoadBalancerId");
        let name = s(lb, "LoadBalancerName");
        let id = format!("lb:{lid}");
        let public = s(lb, "LoadBalancerType") == "OPEN";
        let mut item = Item { id: id.clone(), kind: "lb".into(), name: if name.is_empty() { lid.clone() } else { name }, scope: region.into(), detail: format!("CLB · {}", if public { "public" } else { "internal" }), state: if s(lb, "Status") == "1" { "running".into() } else { "creating".into() }, refs: vec![lid.clone(), s(lb, "Domain"), s(lb, "LoadBalancerDomain")], public, ..Default::default() };
        strings(lb, "LoadBalancerVips").into_iter().for_each(|a| push(&mut item.ips, a));
        item.attrs.push(("VPC".into(), s(lb, "VpcId")));
        item.refs.retain(|r| !r.is_empty());
        for l in d.targets.iter().filter(|x| x.0 == lid).flat_map(|x| list(&x.1, "Listeners")) {
            let port = num(l, "Port");
            let proto = s(l, "Protocol").to_lowercase();
            item.ports.push((port, proto.clone(), String::new()));
            if public {
                inv.link("internet", format!("id:{id}"), port, "");
            }
            // Layer 4: targets on the listener. Layer 7: per rule (domain + path).
            let mut groups: Vec<(String, &Value)> = vec![(format!("listener {port}"), l)];
            for r in list(l, "Rules") {
                groups.push((format!("listener {port} · {}{}", s(r, "Domain"), s(r, "Url")), r));
            }
            for (note, g) in groups {
                for t in list(g, "Targets") {
                    let to = match s(t, "InstanceId") {
                        i if !i.is_empty() => format!("ref:{i}"),
                        _ => format!("ip:{}", strings(t, "PrivateIpAddresses").into_iter().next().unwrap_or_default()),
                    };
                    if to != "ip:" {
                        inv.link(format!("id:{id}"), to, num(t, "Port"), note.clone());
                    }
                }
            }
        }
        inv.items.push(item);
    }

    let mut nat_id: HashMap<String, String> = HashMap::new();
    for n in list(&d.nats, "NatGatewaySet") {
        let gw = s(n, "NatGatewayId");
        if gw.is_empty() {
            continue;
        }
        let id = format!("nat:{gw}");
        let name = s(n, "NatGatewayName");
        let mut item = Item { id: id.clone(), kind: "nat".into(), name: if name.is_empty() { gw.clone() } else { name }, scope: region.into(), detail: "NAT gateway".into(), state: s(n, "State").to_lowercase(), refs: vec![gw.clone()], ..Default::default() };
        for a in list(n, "PublicIpAddressSet") {
            push(&mut item.ips, s(a, "PublicIpAddress"));
        }
        item.attrs.push(("VPC".into(), s(n, "VpcId")));
        inv.link(format!("id:{id}"), "internet", 0, "outbound");
        nat_id.insert(gw, id);
        inv.items.push(item);
    }
    for rt in list(&d.routes, "RouteTableSet") {
        let Some(nat) = list(rt, "RouteSet").into_iter().filter(|r| s(r, "GatewayType") == "NAT").find_map(|r| nat_id.get(&s(r, "GatewayId"))) else { continue };
        for a in list(rt, "AssociationSet") {
            for vm in by_subnet.get(&s(a, "SubnetId")).into_iter().flatten() {
                inv.link(format!("id:{vm}"), format!("id:{nat}"), 0, format!("internet access of {}", s(a, "SubnetId")));
            }
        }
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvm_clb_and_nat() {
        let d = Region {
            instances: json!({ "TotalCount": 2, "InstanceSet": [
                { "InstanceId": "ins-web", "InstanceName": "web-1", "InstanceType": "S5.MEDIUM4", "InstanceState": "RUNNING", "PrivateIpAddresses": ["10.0.1.5"], "PublicIpAddresses": ["43.156.1.2"], "Placement": { "Zone": "ap-singapore-1" }, "VirtualPrivateCloud": { "VpcId": "vpc-1", "SubnetId": "subnet-a" } },
                { "InstanceId": "ins-app", "InstanceName": "", "InstanceType": "S5.LARGE8", "InstanceState": "RUNNING", "PrivateIpAddresses": ["10.0.2.7"], "PublicIpAddresses": null, "Placement": { "Zone": "ap-singapore-2" }, "VirtualPrivateCloud": { "VpcId": "vpc-1", "SubnetId": "subnet-b" } }
            ] }),
            lbs: json!({ "LoadBalancerSet": [{ "LoadBalancerId": "lb-1", "LoadBalancerName": "shop", "LoadBalancerType": "OPEN", "LoadBalancerVips": ["119.28.1.1"], "Status": 1, "VpcId": "vpc-1", "Domain": "" }] }),
            targets: vec![("lb-1".into(), json!({ "Listeners": [
                { "ListenerId": "lbl-1", "Protocol": "HTTPS", "Port": 443, "Rules": [{ "Domain": "shop.example.com", "Url": "/", "Targets": [{ "InstanceId": "ins-app", "Port": 8080, "PrivateIpAddresses": ["10.0.2.7"] }] }], "Targets": null },
                { "ListenerId": "lbl-2", "Protocol": "TCP", "Port": 22, "Targets": [{ "Type": "ENI", "InstanceId": "", "Port": 22, "PrivateIpAddresses": ["10.0.9.9"] }] }
            ] }))],
            nats: json!({ "NatGatewaySet": [{ "NatGatewayId": "nat-1", "NatGatewayName": "egress", "State": "AVAILABLE", "VpcId": "vpc-1", "PublicIpAddressSet": [{ "PublicIpAddress": "119.28.9.9" }] }] }),
            routes: json!({ "RouteTableSet": [{ "RouteSet": [{ "GatewayType": "NAT", "GatewayId": "nat-1", "DestinationCidrBlock": "0.0.0.0/0" }], "AssociationSet": [{ "SubnetId": "subnet-b" }] }] }),
        };
        let inv = parse("ap-singapore", &d);
        let item = |id: &str| inv.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!((item("vm:ins-web").ips.clone(), item("vm:ins-web").public, item("vm:ins-web").state.as_str(), item("vm:ins-app").name.as_str()), (vec!["10.0.1.5".to_string(), "43.156.1.2".to_string()], true, "running", "ins-app"));
        assert_eq!((item("lb:lb-1").ips.clone(), item("lb:lb-1").public, item("lb:lb-1").ports.iter().map(|p| p.0).collect::<Vec<_>>(), item("lb:lb-1").refs.clone()), (vec!["119.28.1.1".to_string()], true, vec![443, 22], vec!["lb-1".to_string()]));
        assert_eq!(item("nat:nat-1").ips, ["119.28.9.9"]);
        let links: Vec<String> = inv.links.iter().map(|l| format!("{} -> {} :{} {}", l.from, l.to, l.port, l.note)).collect();
        assert_eq!(
            links,
            [
                "internet -> id:lb:lb-1 :443 ",
                "id:lb:lb-1 -> ref:ins-app :8080 listener 443 · shop.example.com/",
                "internet -> id:lb:lb-1 :22 ",
                "id:lb:lb-1 -> ip:10.0.9.9 :22 listener 22",
                "id:nat:nat-1 -> internet :0 outbound",
                "id:vm:ins-app -> id:nat:nat-1 :0 internet access of subnet-b",
            ]
        );
    }
}
