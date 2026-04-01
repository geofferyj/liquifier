fn main() {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile(&["../proto/liquifier.proto"], &["../proto"])
        .expect("Failed to compile protos");
}
