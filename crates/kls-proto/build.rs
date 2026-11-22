fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "../../proto/keleustes/node/v1/node.proto",
        "../../proto/keleustes/node/v1/host.proto",
    ];
    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }
    tonic_prost_build::configure().compile_protos(&protos, &["../../proto"])?;
    Ok(())
}
