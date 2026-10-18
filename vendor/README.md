# Haiku Vulkan support

`wgpu`, `wgpu-core` and `wgpu-core-deps-windows-linux-android` 30.0.1 are copied from crates.io, with their upstream
MIT/Apache-2.0 licenses. The only changes add `target_os = "haiku"` alongside
FreeBSD in platform dependency and backend cfg lists (`Cargo.toml`,
`Cargo.toml.orig`, `build.rs`). This makes their existing Vulkan backend
available on air/OS. There is no new Vulkan implementation or window surface;
the native host renders offscreen and presents the completed bitmap.

`cap-primitives` 4.0.3 (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT)
adds Haiku directory-entry metadata lookup, no-O_PATH handling, signed inode/link
count conversion, Haiku's signed `st_rdev = -1` sentinel for ordinary files,
and an explicit unsupported error for O_RSYNC. A target-side regression test
covers that sentinel (upstream's unsigned conversion panicked). It preserves
capability-relative traversal and symlink checks; no automation permission is bypassed.

The `cap-tempfile = "4"` development dependency is restored so the published
crate's native tests can run. Its upstream path-only entry was omitted from the
crates.io manifest. From the repository root, build the target test executable:

```sh
. tools/haiku/env.sh
cargo test --manifest-path vendor/cap-primitives/Cargo.toml \
  --config "patch.crates-io.cap-primitives.path=\"$PWD/vendor/cap-primitives\"" \
  --release --target x86_64-unknown-haiku -Zbuild-std=std,panic_abort --lib --no-run
```

Run the resulting `cap_primitives-*` executable on Haiku; all 16 tests passed on
both validation machines.
