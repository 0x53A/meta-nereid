# rust-native uses LLVM to compile the embedded WebAssembly demo from source.
LLVM_TARGETS_TO_BUILD:append:class-native = ";WebAssembly"
