//! AWS: EC2 instances, load balancers (ALB / NLB / GWLB with their target
//! groups, listeners and target health) and NAT gateways with the subnets that
//! route through them. Through the `aws` CLI on a server, or the EC2 / ELB
//! APIs signed with a key from the vault.

use std::collections::HashMap;

use serde_json::Value;

use super::fetch::Via;
use super::{list, num, push, s, Inventory, Item, Source};
use crate::error::AppResult;

#[derive(Default)]
pub struct Region {
    pub instances: Value,
    pub lbs: Value,
    pub tgs: Value,
    /// (load balancer ARN, its listeners)
    pub listeners: Vec<(String, Value)>,
    /// (target group ARN, its targets' health)
    pub health: Vec<(String, Value)>,
    pub nats: Value,
    pub routes: Value,
}

/// One describe call: `aws <cli…>` or `Action=<action>` on the service's endpoint.
async fn call(via: &Via, src: &Source, region: &str, service: &str, cli: &[&str], action: &str, params: &[(&str, &str)]) -> AppResult<Value> {
    if via.is_cli() {
        let mut args: Vec<&str> = vec![if service == "ec2" { "ec2" } else { "elbv2" }];
        args.extend(cli);
        args.extend(["--region", region, "--output", "json", "--no-cli-pager"]);
        return via.cli("aws", &args).await;
    }
    let version = if service == "ec2" { "2016-11-15" } else { "2015-12-01" };
    via.aws(&src.endpoint, service, region, action, version, params).await
}

async fn region(via: &Via, src: &Source, r: &str) -> AppResult<Region> {
    let elb = "elasticloadbalancing";
    let mut d = Region {
        instances: call(via, src, r, "ec2", &["describe-instances"], "DescribeInstances", &[]).await?,
        lbs: call(via, src, r, elb, &["describe-load-balancers"], "DescribeLoadBalancers", &[]).await?,
        tgs: call(via, src, r, elb, &["describe-target-groups"], "DescribeTargetGroups", &[]).await?,
        nats: call(via, src, r, "ec2", &["describe-nat-gateways"], "DescribeNatGateways", &[]).await?,
        routes: call(via, src, r, "ec2", &["describe-route-tables"], "DescribeRouteTables", &[]).await?,
        ..Default::default()
    };
    for lb in list(&d.lbs, "LoadBalancers").into_iter().take(40) {
        let arn = s(lb, "LoadBalancerArn");
        if let Ok(v) = call(via, src, r, elb, &["describe-listeners", "--load-balancer-arn", &arn], "DescribeListeners", &[("LoadBalancerArn", &arn)]).await {
            d.listeners.push((arn, v));
        }
    }
    for tg in list(&d.tgs, "TargetGroups").into_iter().take(60) {
        let arn = s(tg, "TargetGroupArn");
        if let Ok(v) = call(via, src, r, elb, &["describe-target-health", "--target-group-arn", &arn], "DescribeTargetHealth", &[("TargetGroupArn", &arn)]).await {
            d.health.push((arn, v));
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
    // Nothing at all could be read: that is the error, not an empty account.
    match failed {
        Some(e) if inv.notes.len() == src.regions.len() => Err(e),
        _ => Ok(inv),
    }
}

fn tag(v: &Value, key: &str) -> String {
    list(v, "Tags").into_iter().find(|t| s(t, "Key") == key).map(|t| s(t, "Value")).unwrap_or_default()
}

pub fn parse(region: &str, d: &Region) -> Inventory {
    let mut inv = Inventory::default();
    // subnet → the instances in it
    let mut by_subnet: HashMap<String, Vec<String>> = HashMap::new();
    for i in list(&d.instances, "Reservations").into_iter().flat_map(|r| list(r, "Instances")) {
        let id = s(i, "InstanceId");
        let state = s(&i["State"], "Name");
        if id.is_empty() || state == "terminated" {
            continue;
        }
        let name = tag(i, "Name");
        let mut item = Item { id: format!("vm:{id}"), kind: "vm".into(), name: if name.is_empty() { id.clone() } else { name }, scope: s(&i["Placement"], "AvailabilityZone"), detail: s(i, "InstanceType"), state, refs: vec![id.clone()], ..Default::default() };
        push(&mut item.ips, s(i, "PrivateIpAddress"));
        push(&mut item.ips, s(i, "PublicIpAddress"));
        for nic in list(i, "NetworkInterfaces") {
            for a in list(nic, "PrivateIpAddresses") {
                push(&mut item.ips, s(a, "PrivateIpAddress"));
            }
        }
        item.public = !s(i, "PublicIpAddress").is_empty();
        for (label, key) in [("Instance", "InstanceId"), ("VPC", "VpcId"), ("Subnet", "SubnetId")] {
            if !s(i, key).is_empty() {
                item.attrs.push((label.into(), s(i, key)));
            }
        }
        by_subnet.entry(s(i, "SubnetId")).or_default().push(item.id.clone());
        inv.items.push(item);
    }

    // target group → (the ports its listeners are on, the load balancers in front)
    let mut tg_ports: HashMap<String, Vec<u16>> = HashMap::new();
    let mut lb_id: HashMap<String, String> = HashMap::new();
    for lb in list(&d.lbs, "LoadBalancers") {
        let (arn, name) = (s(lb, "LoadBalancerArn"), s(lb, "LoadBalancerName"));
        let id = format!("lb:{name}");
        let public = s(lb, "Scheme") == "internet-facing";
        let mut item = Item { id: id.clone(), kind: "lb".into(), name, scope: region.into(), detail: format!("{} load balancer", s(lb, "Type")), state: s(&lb["State"], "Code"), refs: vec![arn.clone(), s(lb, "DNSName")], public, ..Default::default() };
        for az in list(lb, "AvailabilityZones") {
            for a in list(az, "LoadBalancerAddresses") {
                push(&mut item.ips, s(a, "IpAddress"));
                push(&mut item.ips, s(a, "PrivateIPv4Address"));
            }
        }
        item.attrs.push(("DNS name".into(), s(lb, "DNSName")));
        item.attrs.push(("VPC".into(), s(lb, "VpcId")));
        for l in d.listeners.iter().filter(|x| x.0 == arn).flat_map(|x| list(&x.1, "Listeners")) {
            let port = num(l, "Port");
            item.ports.push((port, s(l, "Protocol").to_lowercase(), String::new()));
            for a in list(l, "DefaultActions") {
                for t in std::iter::once(s(a, "TargetGroupArn")).chain(list(&a["ForwardConfig"], "TargetGroups").into_iter().map(|g| s(g, "TargetGroupArn"))) {
                    if !t.is_empty() && !tg_ports.entry(t.clone()).or_default().contains(&port) {
                        tg_ports.get_mut(&t).expect("entry").push(port);
                    }
                }
            }
            if public {
                inv.link("internet", format!("id:{id}"), port, "");
            }
        }
        lb_id.insert(arn, id);
        inv.items.push(item);
    }
    for tg in list(&d.tgs, "TargetGroups") {
        let arn = s(tg, "TargetGroupArn");
        let fronts: Vec<&String> = list(tg, "LoadBalancerArns").into_iter().filter_map(|a| lb_id.get(a.as_str().unwrap_or_default())).collect();
        let listens = tg_ports.get(&arn).map(|p| p.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        for h in d.health.iter().filter(|x| x.0 == arn).flat_map(|x| list(&x.1, "TargetHealthDescriptions")) {
            let target = s(&h["Target"], "Id");
            let port = Some(num(&h["Target"], "Port")).filter(|p| *p > 0).unwrap_or_else(|| num(tg, "Port"));
            // An instance id, or (target type `ip`) an address.
            let to = if target.starts_with("i-") { format!("ref:{target}") } else { format!("ip:{target}") };
            let note = [format!("target group {}", s(tg, "TargetGroupName")), if listens.is_empty() { String::new() } else { format!("listener {listens}") }, s(&h["TargetHealth"], "State")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
            for lb in &fronts {
                inv.link(format!("id:{lb}"), to.clone(), port, note.clone());
            }
        }
    }

    // NAT gateways, and which subnets send their internet traffic through each.
    let mut nat_id: HashMap<String, String> = HashMap::new();
    for n in list(&d.nats, "NatGateways") {
        let gw = s(n, "NatGatewayId");
        let state = s(n, "State");
        if gw.is_empty() || state == "deleted" {
            continue;
        }
        let name = tag(n, "Name");
        let id = format!("nat:{gw}");
        let mut item = Item { id: id.clone(), kind: "nat".into(), name: if name.is_empty() { gw.clone() } else { name }, scope: region.into(), detail: "NAT gateway".into(), state, refs: vec![gw.clone()], ..Default::default() };
        for a in list(n, "NatGatewayAddresses") {
            push(&mut item.ips, s(a, "PublicIp"));
            push(&mut item.ips, s(a, "PrivateIp"));
        }
        item.attrs.push(("VPC".into(), s(n, "VpcId")));
        item.attrs.push(("In subnet".into(), s(n, "SubnetId")));
        if s(n, "ConnectivityType") != "private" {
            inv.link(format!("id:{id}"), "internet", 0, "outbound");
        }
        nat_id.insert(gw, id);
        inv.items.push(item);
    }
    for rt in list(&d.routes, "RouteTables") {
        let Some(nat) = list(rt, "Routes").into_iter().find_map(|r| nat_id.get(&s(r, "NatGatewayId"))) else { continue };
        for a in list(rt, "Associations") {
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
    use serde_json::json;

    #[test]
    fn instances_load_balancers_and_nat() {
        let d = Region {
            instances: json!({ "Reservations": [{ "Instances": [
                { "InstanceId": "i-0web", "InstanceType": "t3.medium", "State": { "Name": "running" }, "PrivateIpAddress": "10.0.1.5", "PublicIpAddress": "54.1.2.3", "SubnetId": "subnet-pub", "VpcId": "vpc-1", "Placement": { "AvailabilityZone": "us-east-1a" }, "Tags": [{ "Key": "Name", "Value": "web-1" }] },
                { "InstanceId": "i-0app", "InstanceType": "t3.large", "State": { "Name": "running" }, "PrivateIpAddress": "10.0.2.7", "SubnetId": "subnet-priv", "VpcId": "vpc-1", "Placement": { "AvailabilityZone": "us-east-1b" }, "NetworkInterfaces": [{ "PrivateIpAddresses": [{ "PrivateIpAddress": "10.0.2.7" }, { "PrivateIpAddress": "10.0.2.8" }] }] },
                { "InstanceId": "i-0gone", "State": { "Name": "terminated" } }
            ] }] }),
            lbs: json!({ "LoadBalancers": [{ "LoadBalancerArn": "arn:lb/web", "LoadBalancerName": "web", "DNSName": "web-1.us-east-1.elb.amazonaws.com", "Scheme": "internet-facing", "Type": "application", "VpcId": "vpc-1", "State": { "Code": "active" } }] }),
            tgs: json!({ "TargetGroups": [{ "TargetGroupArn": "arn:tg/app", "TargetGroupName": "app", "Port": 8080, "LoadBalancerArns": ["arn:lb/web"] }] }),
            listeners: vec![("arn:lb/web".into(), json!({ "Listeners": [{ "Port": 443, "Protocol": "HTTPS", "DefaultActions": [{ "Type": "forward", "TargetGroupArn": "arn:tg/app" }] }] }))],
            health: vec![("arn:tg/app".into(), json!({ "TargetHealthDescriptions": [{ "Target": { "Id": "i-0app", "Port": 8080 }, "TargetHealth": { "State": "healthy" } }, { "Target": { "Id": "10.0.9.9", "Port": 9000 }, "TargetHealth": { "State": "unhealthy" } }] }))],
            nats: json!({ "NatGateways": [{ "NatGatewayId": "nat-1", "State": "available", "SubnetId": "subnet-pub", "VpcId": "vpc-1", "NatGatewayAddresses": [{ "PublicIp": "52.9.9.9", "PrivateIp": "10.0.1.200" }] }] }),
            routes: json!({ "RouteTables": [{ "Routes": [{ "DestinationCidrBlock": "0.0.0.0/0", "NatGatewayId": "nat-1" }], "Associations": [{ "SubnetId": "subnet-priv" }] }, { "Routes": [{ "GatewayId": "igw-1" }], "Associations": [{ "SubnetId": "subnet-pub" }] }] }),
        };
        let inv = parse("us-east-1", &d);
        let item = |id: &str| inv.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(inv.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["vm:i-0web", "vm:i-0app", "lb:web", "nat:nat-1"], "no terminated instance");
        let web = item("vm:i-0web");
        assert_eq!((web.name.as_str(), web.ips.clone(), web.public, web.detail.as_str(), web.scope.as_str(), web.refs.clone()), ("web-1", vec!["10.0.1.5".to_string(), "54.1.2.3".to_string()], true, "t3.medium", "us-east-1a", vec!["i-0web".to_string()]));
        assert_eq!(item("vm:i-0app").ips, ["10.0.2.7", "10.0.2.8"]);
        let lb = item("lb:web");
        assert_eq!((lb.public, lb.ports.clone(), lb.detail.as_str()), (true, vec![(443, "https".to_string(), String::new())], "application load balancer"));
        assert!(lb.refs.contains(&"web-1.us-east-1.elb.amazonaws.com".to_string()), "found by its DNS name (a Kubernetes service points at it)");
        assert_eq!(item("nat:nat-1").ips, ["52.9.9.9", "10.0.1.200"]);
        let links: Vec<String> = inv.links.iter().map(|l| format!("{} -> {} :{} {}", l.from, l.to, l.port, l.note)).collect();
        assert_eq!(
            links,
            [
                "internet -> id:lb:web :443 ",
                "id:lb:web -> ref:i-0app :8080 target group app · listener 443 · healthy",
                "id:lb:web -> ip:10.0.9.9 :9000 target group app · listener 443 · unhealthy",
                "id:nat:nat-1 -> internet :0 outbound",
                "id:vm:i-0app -> id:nat:nat-1 :0 internet access of subnet-priv",
            ]
        );
    }
}
