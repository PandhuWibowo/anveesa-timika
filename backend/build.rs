// Compiles the gRPC contracts in ../proto (the single source of truth, shared
// with the frontend). protoc is vendored so no system install is needed.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    let protos = ["sys", "kv", "auth", "bastion", "cluster", "audit", "automation", "monitor", "containers"].map(|p| format!("../proto/timika/v1/{p}.proto"));
    tonic_build::configure().build_server(true).build_client(true).compile_protos(&protos, &["../proto"])?;
    println!("cargo:rerun-if-changed=../proto");
    Ok(())
}
