# GPUI as a dependency

Research for issue #10 (map #9). Checked 2026-10-06 against crates.io, the Zed repo (`main`),
the gpui-kit repo (`main`) and docs.rs. Version numbers will go stale quickly; re-check before
pinning.

## Answer

Depend on GPUI through **gpui-kit's `gpui-pre` snapshots on crates.io, pinned exactly**, and
use **`gpui-component`** for widgets:

```toml
# workspace Cargo.toml
gpui = { package = "gpui-pre", version = "=0.3.8" }
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.8", features = ["font-kit", "wayland", "x11"] }
gpui-component = "=0.7.1"
```

- Raise the workspace `rust-version` from 1.89 to at least **1.95**. Track Zed's pinned toolchain,
  1.98.1 today.
- Keep tokio as the owner of all gRPC work. Build the runtime by hand rather than with
  `#[tokio::main]`, give GPUI the main thread, and bridge with a roughly 30-line `Tokio::spawn`
  helper copied from Zed's `gpui_tokio`.
- Fallback: a Zed git dependency pinned to a `rev`. It is the same code, but it costs a clone of
  Zed's large repo in CI, and its GPUI types cannot be shared with `gpui-component`.

## 1. Source and pinning

| Option | State | Verdict |
|---|---|---|
| `gpui` on crates.io (Zed-owned) | Latest is 0.2.2 from 2025-10-22, almost a year old. It predates the platform split into `gpui_platform`/`gpui_linux`/`gpui_windows`/`gpui_wgpu`. [crates.io][gpui-crate] | Stale. Don't use. |
| Zed git, `rev = "…"` | `gpui` on `main` has `version = "0.2.2"`, `publish = true`. The platform crates are `publish.workspace = true`, and the workspace says `publish = false`. [gpui Cargo.toml][gpui-toml] [workspace][zed-toml] | Works, but needs a full clone of Zed in CI, and `gpui-component` can't use these types. |
| `gpui-pre` (crates.io) | A republish of Zed `main` snapshots by the gpui-kit maintainer (crates.io owner `huacnlee`), not by Zed. 0.3.8 is "snapshot of zed@279fe07" (2026-10-05). Releases came out weekly from 0.3.0 (2026-09-03) to 0.3.8. [crates.io][gpui-pre] [owners][gpui-pre-owners] | **Recommended.** It is what `gpui-component`/`gpui-kit` 0.7.x pin. |
| `gpui-ce` (community fork) | 0.3.2/0.3.3 were yanked on 2026-08-28. The fork now diverges on purpose. [runner#733][runner] | Avoid. |

How `gpui-pre` is made ([gpui-kit PR #2929][pr2929], merged 2026-09-03):

- A script publishes any Zed commit under renamed packages: `gpui`→`gpui-pre`, `gpui_platform`→`gpui-pre-platform`, and so on.
- Each crate keeps its `[lib] name`, so `use gpui::*` still works.
- Optional git-only dependencies (`proptest`, `async-tar`) are dropped, and Zed's reqwest fork becomes `gpui-pre-reqwest`.
- The PR describes a biweekly cron. crates.io actually shows weekly releases.

API stability:

- Zed's README says GPUI "is still pre-1.0. There will often be breaking changes between versions." It also says you need "the latest version of stable Rust". [README][gpui-readme]
- gpui-kit pins `gpui-pre` with `=` because "any snapshot may change GPUI's API". A caret requirement once moved apps onto a snapshot that `gpui-component` didn't compile with (gpui-kit #3156). [gpui-kit Cargo.toml][kit-toml]
- **Pin every `gpui-pre*` crate with `=`, and bump them together with `gpui-component`.** Other apps do the same, for example [runner#733][runner].

Trust note: `gpui-pre` is a third-party republish with rewritten manifests. One downstream user
checked the sources file by file against the stated Zed commit and found only the disclosed
manifest and path changes (eidola PR #373, per web search; not re-verified here).

## 2. Rust version and edition

- Neither `gpui-pre` nor `gpui-component` declares a `rust-version`. Both use **edition 2024**. Zed's workspace has no `rust-version` either. [crates.io][gpui-pre] [zed-toml]
- Zed pins its toolchain to **1.98.1** in [`rust-toolchain.toml`][zed-toolchain].
- **Tested here:** a scratch crate with `gpui-pre =0.3.8`, `gpui-pre-platform =0.3.8` and `gpui-component =0.7.1`:
  - It **fails** `cargo check` on Rust 1.93.0 (`E0658: use of unstable library feature cold_path`). `std::hint::cold_path` was stabilized in [1.95.0][cold-path].
  - It **passes** `cargo check` on 1.98.1 (Linux/WSL, about 4.5 minutes cold).
- So Commandant's `rust-version = "1.89"` must rise to at least 1.95. Each snapshot bump can raise it again, so set it to the toolchain CI actually uses (1.98 today).
- Edition 2024 already matches.

## 3. Rendering backends and system dependencies

### Linux

- At least one windowing feature is required: `wayland`, `x11`, or both. Pass them to `gpui_platform`. [README][gpui-readme]
- Zed picks the backend at runtime, and `WAYLAND_DISPLAY=''` forces X11. [linux.md][zed-linux]
- Rendering: `gpui_linux` → `gpui_wgpu` → **wgpu 29**. On Linux it enables the `vulkan` and `gles` backends, so a **Vulkan** driver is the normal path and GLES is a fallback. Text uses cosmic-text, swash and font-kit. [gpui_linux][gpui-linux-toml] [gpui_wgpu][gpui-wgpu-toml]
- Wayland uses `wayland-backend` with `dlopen`. X11 uses pure-Rust `x11rb` plus `xkbcommon`. Portals come through `ashpd` (`xdg-desktop-portal`).
- Zed's own `script/linux` installs these build packages ([script][zed-script-linux]); some exist for Zed itself rather than GPUI:
  - Debian/Ubuntu: `libxkbcommon-x11-dev libx11-xcb-dev libwayland-dev libfontconfig-dev libvulkan1 libglib2.0-dev xdg-desktop-portal`, plus others.
  - Fedora: `libxkbcommon-x11-devel libxcb-devel wayland-devel fontconfig-devel vulkan-loader`.
  - Commandant only needs the GPUI subset: xkbcommon, xcb, wayland, fontconfig and a Vulkan loader.
- Not verified: the probe was `cargo check` only, which neither links nor runs. Do one real `cargo build` and a window launch on a clean distro image before relying on this list.

### Windows

- No features are needed. Windowing is Win32 and text is DirectWrite. [README][gpui-readme]
- The renderer is **Direct3D 11** with DirectComposition (`directx_renderer.rs`, HLSL shaders). It asks for feature level 11_1, 11_0 or 10_1, and 10_1 also needs structured-buffer support. There is no explicit WARP fallback. [gpui_windows Cargo.toml][gpui-windows-toml] [directx_devices.rs][dx-devices]
- Zed's `development/windows.md` still says "Zed currently uses Vulkan as its graphics API on Windows". The code says otherwise, so that doc is stale. [windows.md][zed-windows]
- Build needs MSVC build tools and a Windows 10/11 SDK of at least 10.0.20348. Zed's CMake requirement comes from wasmtime, which GPUI doesn't use. [windows.md][zed-windows]
- `gpui_platform` turns on gpui's `windows-manifest` feature, which uses `embed-resource` at build time. [gpui_platform][gpui-platform-toml]

### macOS (later)

- Rendering is Metal. Enable `font-kit` or no glyphs are drawn. Full Xcode is required, not just the CLT. [README][gpui-readme]

## 4. Executor and tokio

- GPUI has its own executors tied to the platform event loop. [executor.rs][executor] [docs.rs][bg-exec]
  - `ForegroundExecutor::spawn(impl Future + 'static) -> Task<R>` runs on the main thread.
  - `BackgroundExecutor::spawn(impl Future + Send + 'static) -> Task<R>` runs on a background thread.
  - In app code these are `cx.spawn` and `cx.background_spawn`.
  - A `Task` is dropped to cancel it, or `.detach()`ed to keep it running.
- GPUI does not depend on tokio. Neither `gpui-pre` nor `gpui-component` pulls it in, and `gpui-component` uses `smol`. [crates.io deps][gpui-pre]
- tonic needs a tokio runtime: `Channel` spawns its I/O task on tokio. So run tokio beside GPUI, as Zed does.
- Zed's bridge is `crates/gpui_tokio`, about 90 lines. [gpui_tokio.rs][gpui-tokio]
  - `init(cx)` builds a 2-worker multi-thread runtime, and `init_from_handle(cx, Handle)` reuses an existing one. Either way the handle is stored as a GPUI global.
  - `Tokio::spawn(cx, fut)` spawns on the tokio handle and awaits the `JoinHandle` inside a GPUI `background_spawn` task.
  - Dropping the GPUI task aborts the tokio task through `abort_handle`.
  - **It is not on crates.io**: there is no `gpui-pre-tokio` (404), and Zed's crates are unpublished. Copy it into the client. It is small, and only the `defer` helper needs replacing.
- Main-thread ownership: `gpui_platform::application().run(...)` must own the process main thread, a hard requirement on macOS. The client currently uses `#[tokio::main]` (`crates/commandant-cli/src/main.rs`). For the GUI path:
  1. Build the runtime with `tokio::runtime::Builder::new_multi_thread()`.
  2. Call `init_from_handle(cx, rt.handle().clone())` inside `run`.
  3. Run CLI/TUI paths with `rt.block_on(...)` as today.
- Pattern for gRPC calls from a view: `cx.spawn(async move |this, cx| { let r = Tokio::spawn(cx, client.call(req))?.await; this.update(cx, …) })`. For server streams, forward tokio-side items to the UI over a `futures`/`async-channel` channel, or loop on `Tokio::spawn` per message.

## 5. Widgets: use `gpui-component`

- Bare GPUI has `div`, `uniform_list`/`list` (virtualized) and text layout. It has no text-input widget, scrollbar or markdown renderer; Zed builds those in its own `editor`/`ui`/`markdown` crates, which are unpublished.
- `gpui-component` 0.7.1 is Apache-2.0, edition 2024, about 16k stars on the repo, and was pushed to 2026-10-06. [repo][kit-repo] [crates.io][gpui-component]
- It covers all three needs:
  - **Text input:** `input/`, with `input.rs`, `textarea.rs`, `editor.rs`, number and OTP inputs. A code editor with Tree-sitter/LSP is included.
  - **Scrolling lists:** `list/` (delegate-based, with loading), `virtual_list.rs` (variable item sizes), `scroll/scrollable.rs`, `table/` and `message_scroller.rs`.
  - **Markdown:** `text::markdown(src) -> TextView`. Parsing lives in `gpui-base`, and optional Tree-sitter highlighting is available for code blocks. ([text/mod.rs][kit-text])
- The repo was renamed `longbridge/gpui-component` → `longbridge/gpui-kit`. `gpui-component` is now the styled layer, `gpui-base` the unstyled one, and the `gpui-kit` crate is an umbrella that re-exports `gpui`, base, component and assets and pins the matching GPUI. [README][kit-repo]
- Setup: call `gpui_component::init(cx)` (or `gpui_kit::init`) before using any component, and wrap windows in `Root` so dialogs, notifications and menus work. [hello_world][kit-hello]
- Cost: it **forces the `gpui-pre` source**, since it pins `gpui-pre =0.3.8` exactly. Every upgrade is a lockstep bump of `gpui-component` plus all `gpui-pre*` crates. That is acceptable for an app, and it is the strongest reason not to use the Zed git route.
- Choice of crate: depend on `gpui-component` directly, or on `gpui-kit = "0.7"` to get the pin managed in one place. Both resolve to the same snapshot.

## Open items for implementation

- Run one real build and launch on Linux (Wayland and X11) and on Windows to confirm runtime libraries and drivers. This research only ran `cargo check` on Linux.
- Build time and binary size are not measured. A cold `cargo check` took about 4.5 minutes on a mid-range WSL box, so consider putting the GUI behind a cargo feature on the client binary so CLI/TUI-only builds skip it.

[gpui-crate]: https://crates.io/crates/gpui
[gpui-pre]: https://crates.io/crates/gpui-pre
[gpui-pre-owners]: https://crates.io/api/v1/crates/gpui-pre/owners
[gpui-component]: https://crates.io/crates/gpui-component
[gpui-toml]: https://github.com/zed-industries/zed/blob/main/crates/gpui/Cargo.toml
[zed-toml]: https://github.com/zed-industries/zed/blob/main/Cargo.toml
[zed-toolchain]: https://github.com/zed-industries/zed/blob/main/rust-toolchain.toml
[gpui-readme]: https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md
[gpui-platform-toml]: https://github.com/zed-industries/zed/blob/main/crates/gpui_platform/Cargo.toml
[gpui-linux-toml]: https://github.com/zed-industries/zed/blob/main/crates/gpui_linux/Cargo.toml
[gpui-wgpu-toml]: https://github.com/zed-industries/zed/blob/main/crates/gpui_wgpu/Cargo.toml
[gpui-windows-toml]: https://github.com/zed-industries/zed/blob/main/crates/gpui_windows/Cargo.toml
[dx-devices]: https://github.com/zed-industries/zed/blob/main/crates/gpui_windows/src/directx_devices.rs
[gpui-tokio]: https://github.com/zed-industries/zed/blob/main/crates/gpui_tokio/src/gpui_tokio.rs
[executor]: https://github.com/zed-industries/zed/blob/main/crates/gpui/src/executor.rs
[bg-exec]: https://docs.rs/gpui-pre/latest/gpui/struct.BackgroundExecutor.html
[zed-linux]: https://github.com/zed-industries/zed/blob/main/docs/src/development/linux.md
[zed-script-linux]: https://github.com/zed-industries/zed/blob/main/script/linux
[zed-windows]: https://github.com/zed-industries/zed/blob/main/docs/src/development/windows.md
[cold-path]: https://doc.rust-lang.org/std/hint/fn.cold_path.html
[kit-repo]: https://github.com/longbridge/gpui-kit
[kit-toml]: https://github.com/longbridge/gpui-kit/blob/main/Cargo.toml
[kit-text]: https://github.com/longbridge/gpui-kit/blob/main/crates/component/src/text/mod.rs
[kit-hello]: https://github.com/longbridge/gpui-kit/blob/main/examples/hello_world/src/main.rs
[pr2929]: https://github.com/longbridge/gpui-kit/pull/2929
[runner]: https://github.com/yicheng47/runner/issues/733
