fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a vendored protoc so building doesn't require a system install.
    // SAFETY: build scripts are single-threaded.
    unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?) };
    tonic_prost_build::configure().compile_protos(&["proto/commandant.proto"], &["proto"])?;
    Ok(())
}
