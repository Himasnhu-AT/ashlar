# ashlar-obfuscate

Compile-time string obfuscation. `obfuscated!` stores a string XOR-ed with a fixed keystream, so the
plaintext doesn't appear in the binary's read-only data, and recovers it at run time.

```rust
let addr = ashlar_obfuscate::obfuscated!("https://example.invalid/v1");
assert_eq!(addr, "https://example.invalid/v1");
```

This is obfuscation, not encryption: the key travels with the program, so anyone running it can
still recover the value. It only raises the bar on a `strings` dump of the file at rest. Use it for
build-time values such as service addresses, not for secrets.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
