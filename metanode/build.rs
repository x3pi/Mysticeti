// Copyright (c) MetaNode Team
// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Build protobuf files
    let mut protos = Vec::new();
    
    // Build transaction.proto
    let tx_proto = std::path::Path::new("proto/transaction.proto");
    if tx_proto.exists() {
        protos.push("proto/transaction.proto");
        println!("cargo:rerun-if-changed=proto/transaction.proto");
    } else {
        eprintln!("Warning: proto/transaction.proto not found, skipping");
    }
    
    // Build executor.proto
    let executor_proto = std::path::Path::new("proto/executor.proto");
    if executor_proto.exists() {
        protos.push("proto/executor.proto");
        println!("cargo:rerun-if-changed=proto/executor.proto");
    } else {
        eprintln!("Warning: proto/executor.proto not found, skipping");
    }
    
    if !protos.is_empty() {
        let out_dir = std::env::var("OUT_DIR").unwrap();
        println!("cargo:warning=Compiling {} protobuf files to {}", protos.len(), out_dir);
        prost_build::Config::new()
            .out_dir(&out_dir)
            .compile_protos(&protos, &["proto"])
            .unwrap_or_else(|e| {
                panic!("Failed to compile protobuf: {}", e);
            });
        println!("cargo:warning=Successfully compiled protobuf files");
    } else {
        panic!("No protobuf files found to compile!");
    }
}
