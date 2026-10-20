Run the native Yurt host contract test against the installed SDK header:

The SDK's `yurt_jit.h` must declare `yurt_jit_reserve` and
`yurt_jit_mount_module_reserved` (toolchain #207). Older SDK headers cannot
compile this test. Use a current SDK header; do not add local declarations.
Keep assertions enabled: the test rejects builds with `NDEBUG`.

```sh
c++ -std=c++17 -Wall -Wextra -Werror -I. -idirafter "$YURT_SDK/include" \
  runtime/NightInprocHostYurt.cpp tests/inproc-host/yurt.cpp \
  -o /tmp/nightmonkey-inproc-host-test
/tmp/nightmonkey-inproc-host-test
cargo test -p night-compiler --lib
```

The C++ test exercises the production host translation unit with substituted
compiler FFI and Yurt calls, including a real fd that is closed and reused.
It covers early rejection of capped reservations, successful mounting, and
failed mounts consuming their reservation without closing a reused fd.
The Rust assembler test uses real Wasmtime shared memory, a funcref table,
and named helper imports; it publishes entries at a reserved base and executes
an indirect call through that table. These tests do not replace an end-to-end
Yurt guest run through the kernel mount syscall.
