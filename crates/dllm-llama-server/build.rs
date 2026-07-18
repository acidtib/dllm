use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=CUDA_PATH");

    if std::env::var_os("CARGO_FEATURE_CUDA").is_none() {
        return;
    }

    // llama-cpp-sys-4 builds static CUDA archives but does not propagate
    // their CUDA toolkit dependencies to the final Rust binary.
    let cuda_path = std::env::var_os("CUDA_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/local/cuda"));
    println!(
        "cargo:rustc-link-search=native={}",
        cuda_path.join("lib64").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        cuda_path.join("lib64/stubs").display()
    );
    println!("cargo:rustc-link-lib=dylib=cudart");
    println!("cargo:rustc-link-lib=dylib=cublas");
    println!("cargo:rustc-link-lib=dylib=cublasLt");
    println!("cargo:rustc-link-lib=dylib=nccl");
    println!("cargo:rustc-link-lib=dylib=cuda");
}
