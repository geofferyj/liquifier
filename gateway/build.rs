fn main() {
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile(&["../proto/liquifier.proto"], &["../proto"])
        .expect("Failed to compile protos");
}
