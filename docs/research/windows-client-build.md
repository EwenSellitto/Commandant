# Windows build of the client binary

Research for [#12](https://github.com/EwenSellitto/Commandant/issues/12) (map #9). Checked 2026-10-06.

**Question:** the developer works in WSL 2 and tests Windows by hand. How do they get a Windows `commandant.exe` for the client binary (clap CLI, ratatui/crossterm TUI, GPUI GUI, tonic/prost gRPC over tokio)?

## Recommendation

**Build natively on Windows with the MSVC toolchain, from a clone on the Windows filesystem.** Cross-compiling from WSL can't produce a working GPUI GUI, because of how GPUI builds its Windows shaders (see below).

One-time setup on Windows:

1. Run `rustup-init.exe`. If Visual Studio is missing, it offers to install it. Otherwise install Visual Studio 2022 (Community, or Build Tools if you have a license) with the **"Desktop development with C++"** workload. That workload provides the MSVC linker and the **Windows SDK**, and the SDK ships `fxc.exe`, which GPUI's release build needs. [rustup: MSVC prereqs]
2. Clone the repo on the Windows side, e.g. `C:\src\Commandant`, rather than building the WSL checkout through `\\wsl$`. [MS: WSL filesystems]

Each build:

```powershell
cd C:\src\Commandant
git pull
cargo build --release -p commandant-cli   # target\release\commandant.exe
```

You can also drive it from WSL through interop, since Windows `.exe`s run from a WSL shell and keep the working directory [MS: WSL filesystems]: `cd /mnt/c/src/Commandant && cargo.exe build --release -p commandant-cli`. This assumes `%USERPROFILE%\.cargo\bin` is on the Windows `PATH` that WSL imports, which rustup sets up. I haven't verified this on this machine.

`cargo-xwin` is still useful for a quick check from WSL that the **non-GUI** code compiles for Windows. That only works if the GUI is behind a cargo feature you can turn off. It is not the way to get the exe you test.

## Why cross-compiling from WSL fails for the GUI

GPUI's Windows renderer is DirectX 11 with HLSL shaders. It handles them in two modes:

- **Release** (`not(debug_assertions)`): the shaders are compiled at build time by running `fxc.exe` in `build.rs`. The renderer then does `include!(concat!(env!("OUT_DIR"), "/shaders_bytes.rs"))`. [gpui 0.2.2 `src/platform/windows/directx_renderer.rs`; zed main `crates/gpui_windows/src/directx_renderer.rs`]
- **Debug**: the shaders are compiled at runtime with `D3DCompileFromFile`, from `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/platform/windows/...").canonicalize()?`. That is a path on the build machine, baked into the binary. [same files]

The build script decides whether to compile the shaders using `#[cfg(target_os = "windows")]` and `#[cfg(not(debug_assertions))]`. In gpui 0.2.2 it reads `CARGO_CFG_TARGET_OS` to dispatch, then calls `windows::build()` only under `#[cfg(target_os = "windows")]`. Zed main's `crates/gpui_windows/build.rs` uses `#[cfg(target_os = "windows")]` directly. [gpui 0.2.2 `build.rs`; zed main `crates/gpui_windows/build.rs`]

Cargo's docs say `#[cfg]` and `cfg!` in a build script "check the _host_ platform, not the _target_". [Cargo: env vars for build scripts] So on a Linux host:

| Profile | Result when cross-compiling to Windows (cargo-xwin or `-gnu`) |
|---|---|
| release | The shader step is compiled out, so `shaders_bytes.rs` is never written and the `include!` **fails to compile**. |
| debug | It builds, but at runtime it tries to `canonicalize()` a Linux path (`/home/...`) on Windows, so **renderer init fails**. |

Also, `fxc.exe` is located through `GPUI_FXC_PATH`, `where.exe`, or the Windows SDK registry key. All of that is Windows-only, and none of it runs on a Linux host. [gpui 0.2.2 `build.rs`] The `windows-manifest` resource embedding (a default feature, via `embed-resource`) sits under the same host `cfg`, so a cross-compiled exe would also lack GPUI's manifest. [gpui 0.2.2 `build.rs`, `Cargo.toml.orig`]

None of this depends on the toolchain: MSVC via cargo-xwin and the MinGW `-gnu` target both hit it. The only ways around it would be patching or forking GPUI's build script, which isn't worth it for hand testing.

## Is the client dependency tree Windows-clean?

Yes. Apart from the GPUI shader step above, nothing is a blocker.

| Dep | Windows status | Source |
|---|---|---|
| Rust target `x86_64-pc-windows-msvc` | Tier 1 with host tools ("guaranteed to work"). `x86_64-pc-windows-gnu` is also Tier 1. | [Rust platform support] |
| GPUI | Windows backend in-tree (DirectX). A native release build needs `fxc.exe` from the Windows SDK. Zed only supports MSVC builds, and its docs say it "does not support unofficial MSYS2 Zed packages built for Mingw-w64". | [Zed: building on Windows], gpui source |
| crossterm 0.29 (`event-stream`) | "supports all UNIX and Windows terminals down to Windows 7". RGB/256 colours need Windows 10+. | crossterm 0.29.0 `README.md` |
| ratatui (`crossterm` backend) | Pure Rust on top of crossterm. | — |
| tonic 0.14 | Default features are `router`, `transport`, `codegen`, with no TLS. That means no `aws-lc-rs`/cmake/nasm toolchain to install. | tonic 0.14.6 `Cargo.toml` |
| prost + `protoc-bin-vendored` 3.2.0 | It picks the binary from `env::consts::OS/ARCH` of the process running the build script, which is the host. On native Windows, `("windows", _) => Win32` gives a 32-bit `protoc.exe`, which runs on x64 via WOW64. When cross-compiling, the Linux protoc is used, which is also fine. | `protoc-bin-vendored-3.2.0/src/lib.rs` |
| tokio (`process`, `signal`, `net`, …) | Builds on Windows. The client code only uses `tokio::signal::ctrl_c()`, which is cross-platform. | repo grep |
| `commandant-common` | Unix-only bits (`OpenOptionsExt::mode`, `PermissionsExt`) are already behind `#[cfg(unix)]` in `src/fs.rs`. | repo |
| `directories` | Has a Windows backend (Known Folders). | — |

`git2` (whose OpenSSL/libssh2 come from pkg-config) and `sqlx` would be the painful ones on Windows. They leave the client along with orchestrator and worker, so they don't matter here.

## Native-build pitfalls

- **Don't build the WSL checkout from Windows** through `\\wsl$\...`. Microsoft says: "If you're working in a Windows command line (PowerShell, Command Prompt), store your files in the Windows file system." Cross-OS file access is slow. Keep a separate Windows clone, which also keeps the two `target/` dirs apart. [MS: WSL filesystems]
- **Debug exes are tied to the machine that built them.** A GPUI debug build reads `.hlsl` files from the source tree at runtime (see above). It works on the machine that built it, but a copied debug exe won't render. Use `--release` for anything you move around.
- **Missing `fxc.exe`** makes the release build panic with "Failed to find fxc.exe". Install the Windows SDK, or point `GPUI_FXC_PATH` at `fxc.exe`. [gpui 0.2.2 `build.rs`]
- **Long paths.** Zed's Windows docs recommend `git config --system core.longpaths true` and enabling `LongPathsEnabled`, followed by a reboot. Apply these if a dependency checkout fails on path length. [Zed: building on Windows]
- **Don't set `RUSTFLAGS` globally.** Zed's docs warn that it overrides `.cargo/config.toml` rustflags and causes hard-to-diagnose failures. [Zed: building on Windows]
- **`rust-lld` `STATUS_ACCESS_VIOLATION`.** Zed reports this from `rust-lld.exe`. The fix is to switch linkers. [Zed: building on Windows]
- **Single binary vs. console.** CLI and TUI need the console subsystem, so don't add `#![windows_subsystem = "windows"]` to the shared binary. The cost is that launching the GUI by double-click also opens a console window. This is a later design choice, not a build blocker. [Rust reference: `windows_subsystem`]
- **Stale docs.** Zed's Windows page still says it "uses Vulkan" on Windows, and gpui's README says to "be on macOS or Linux". The code in gpui 0.2.2 and in Zed main ships a DirectX 11 renderer, so trust the code.

## Note on which GPUI

crates.io `gpui` is at 0.2.2 (2025-10-22), and the Windows code is inside the `gpui` crate. Zed `main` has since split it into `crates/gpui_windows`, which is not published. Both versions have the same host-`cfg` shader logic, so the conclusion holds whichever one the client depends on.

## CI (later, out of scope)

A `windows-latest` GitHub runner has VS and the Windows SDK, so it would do the same native build. That fits once CI work starts. It isn't needed for hand testing.

## Sources

- [Rust platform support]: https://doc.rust-lang.org/nightly/rustc/platform-support.html
- [rustup: MSVC prereqs]: https://rust-lang.github.io/rustup/installation/windows-msvc.html
- [Cargo: env vars for build scripts]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
- [Zed: building on Windows]: https://github.com/zed-industries/zed/blob/main/docs/src/development/windows.md
- gpui 0.2.2 crate source (`build.rs`, `Cargo.toml.orig`, `README.md`, `src/platform/windows/directx_renderer.rs`): https://static.crates.io/crates/gpui/gpui-0.2.2.crate
- Zed main `crates/gpui_windows/build.rs`: https://github.com/zed-industries/zed/blob/main/crates/gpui_windows/build.rs, and `src/directx_renderer.rs` in the same crate
- cargo-xwin README: https://github.com/rust-cross/cargo-xwin
- [MS: WSL filesystems]: https://learn.microsoft.com/en-us/windows/wsl/filesystems
- [Rust reference: `windows_subsystem`]: https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute
- Local crate sources: `protoc-bin-vendored-3.2.0/src/lib.rs`, `crossterm-0.29.0/README.md`, `tonic-0.14.6/Cargo.toml`
