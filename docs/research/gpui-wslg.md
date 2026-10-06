# Running a GPUI app under WSLg

Answers [#11](https://github.com/EwenSellitto/Commandant/issues/11). Researched 2026-10-06 against Zed `main` at
[`e7526b3`](https://github.com/zed-industries/zed/tree/e7526b378acfa41cf6db0a89aacb31531b1fa087) (wgpu 29).

## Short answer

Yes, it runs. GPUI on Linux renders through **wgpu with the Vulkan and OpenGL backends both enabled**. On WSLg the
GPU path is **OpenGL through Mesa's d3d12 Gallium driver**: set `GALLIUM_DRIVER=d3d12`. You do not need dozen, the
Vulkan driver.

- **Works out of the box, but in software.** With no env vars, the only adapters are lavapipe (Vulkan) and llvmpipe
  (GL), and both run on the CPU. GPUI still opens windows. It is just slow.
- **What the GPU needs:** `export GALLIUM_DRIVER=d3d12`. On this machine that turns GL into
  `D3D12 (NVIDIA GeForce RTX 2070)`, accelerated, GL 4.6 / GLES 3.1. GPUI's adapter ranking then picks it over lavapipe.
- **Dozen (Vulkan on D3D12) is optional and not worth it.** Fedora's `mesa-vulkan-drivers` does not ship it. Mesa calls
  it non-conformant, and users report crashes with it on NVIDIA under WSL.
- **Fallbacks, in order:**
  1. the d3d12 GL driver;
  2. software rendering (lavapipe/llvmpipe), which is automatic;
  3. `WAYLAND_DISPLAY=` to run through XWayland if the Wayland path misbehaves;
  4. run the GUI as a native Windows build.

## How GPUI picks a backend and GPU on Linux

**Windowing.** `select_backend` picks Wayland if `WAYLAND_DISPLAY` is set and not empty. Otherwise it picks X11 if
`DISPLAY` is set. Otherwise it runs headless.
([`gpui_linux/src/linux/display_connection.rs`](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_linux/src/linux/display_connection.rs#L23-L35))
On WSLg both are set, so GPUI uses Wayland (Weston). `WAYLAND_DISPLAY=` forces XWayland, which is the fallback Zed's
own docs suggest ([Zed Linux docs](https://zed.dev/docs/linux)).

**Renderer.** Zed replaced blade (Vulkan only) with wgpu in
[zed#46758](https://github.com/zed-industries/zed/pull/46758), merged 2026-02-13. The wgpu instance is created with
`Backends::VULKAN | Backends::GL`
([`wgpu_context.rs` L471-480](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_wgpu/src/wgpu_context.rs#L471-L480)).
The crate builds wgpu with the `gles` and `vulkan` features
([`gpui_wgpu/Cargo.toml`](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_wgpu/Cargo.toml)).

**Adapter choice** ([`wgpu_context.rs` L497-637](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_wgpu/src/wgpu_context.rs#L497-L637)):

1. GPUI lists every adapter and sorts them by four keys, in this order:
   - a `ZED_DEVICE_ID` match;
   - the compositor's GPU;
   - device type: Discrete > Integrated > Other > Virtual > **Cpu**;
   - backend: Vulkan/Metal/Dx12 > GL.
2. It tries each adapter in turn. The first one that creates a device and configures the window surface wins.
3. If none works, it fails with `No GPU adapter found that can configure the display surface`.

Software adapters are accepted when the window is first created. They are skipped only when the device is recreated
after a GPU device-lost event (`new_rejecting_software`,
[`wgpu_renderer.rs` L2235-2243](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_wgpu/src/wgpu_renderer.rs#L2235-L2243)).

**How wgpu labels GL adapters.** OpenGL cannot report a device type, so wgpu guesses it from the renderer string. A
string containing `llvmpipe` becomes `Cpu`. A string matching none of its integrated or CPU patterns becomes `Other`
([wgpu v29 `gles/adapter.rs` L109-159](https://github.com/gfx-rs/wgpu/blob/v29.0.0/wgpu-hal/src/gles/adapter.rs#L109-L159)).
So `D3D12 (NVIDIA GeForce RTX 2070)` is `Other`, which outranks lavapipe (`Cpu`). That is how the d3d12 GL adapter
wins without any extra configuration.

**The "emulated GPU" prompt is Zed's, not GPUI's.** GPUI only reports `GpuSpecs { is_software_emulated, .. }`
([`wgpu_renderer.rs` L1109-1117](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_wgpu/src/wgpu_renderer.rs#L1109-L1117)).
The Zed app is what shows the "Unsupported GPU" dialog, and `ZED_ALLOW_EMULATED_GPU` turns it off
([`zed/src/zed.rs` L732-774](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/zed/src/zed.rs#L732-L774)).
A GPUI app of our own will not show that dialog. Read `window.gpu_specs()` if we want to warn the user ourselves.

**Env vars GPUI reads:**
- `ZED_DEVICE_ID` (hex device id), to force an adapter;
- `ZED_HEADLESS`, to force headless mode
  ([`gpui_linux/src/linux.rs` L38-43](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_linux/src/linux.rs#L38-L43)).

To see which adapter was chosen, run with `RUST_LOG=info` (or `ZED_LOG=wgpu=info` in Zed). Look for `Found N GPU
adapter(s)` and `Selected GPU (passed configuration test)`.

## The WSLg side

- WSLg is Weston, with XWayland, remoted over RDP. GPU acceleration comes from **Mesa's d3d12 Gallium driver**
  (OpenGL on D3D12), which has been upstream since Mesa 21.0. It only helps if your distro builds that driver
  ([microsoft/wslg README](https://github.com/microsoft/wslg)).
- On a discrete GPU, frames are copied through system memory to reach Weston, which costs some overhead
  ([microsoft/wslg README](https://github.com/microsoft/wslg)).
- Mesa describes d3d12 as "a Gallium driver that emits API calls for Microsoft's D3D12 API". It has
  `MESA_D3D12_DEFAULT_ADAPTER_NAME` to pick an adapter by name
  ([Mesa d3d12 docs](https://docs.mesa3d.org/drivers/d3d12.html)).
- **Dozen** is the Vulkan driver, built with `-Dvulkan-drivers=microsoft-experimental`
  ([Mesa `meson.options`](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/meson.options)). It calls
  `vk_warn_non_conformant_implementation("dzn")` at startup
  ([`src/microsoft/vulkan/dzn_device.c`](https://gitlab.freedesktop.org/mesa/mesa/-/blob/main/src/microsoft/vulkan/dzn_device.c)).
  There are recent reports of it segfaulting in `vkCreateDevice` on NVIDIA under WSLg
  ([steelbrain/reims-vgpu#32](https://github.com/steelbrain/reims-vgpu/issues/32)).

## What this machine has (checked 2026-10-06, read-only)

| Item | Value |
|---|---|
| Distro | Fedora Linux 42 (WSL) |
| Mesa | 25.1.9: `mesa-dri-drivers`, `mesa-libEGL`, `mesa-libGL`, `mesa-vulkan-drivers`, `vulkan-loader` 1.4.313 |
| d3d12 GL driver | present (`/usr/lib64/dri/d3d12_dri.so`) |
| Vulkan ICDs | asahi, broadcom, freedreno, intel, intel_hasvk, **lvp (lavapipe)**, nouveau, panfrost, powervr, radeon, virtio. **No dozen (`dzn`)** |
| WSL libs | `libd3d12.so`, `libd3d12core.so`, `libdxcore.so` are on the `ldconfig` path |
| `glxinfo -B` (default) | `llvmpipe (LLVM 20.1.8)`, Accelerated: **no** |
| `GALLIUM_DRIVER=d3d12 glxinfo -B` | `D3D12 (NVIDIA GeForce RTX 2070)`, Accelerated: **yes**, GL 4.6, GLES 3.1 |

So with no env vars, Mesa uses llvmpipe even though d3d12 is installed. `GALLIUM_DRIVER=d3d12` is the one switch
that turns the GPU on.

## Field reports (Zed on WSLg)

- [zed#38116](https://github.com/zed-industries/zed/issues/38116): Zed hung on launch under WSLg. The maintainers said
  running Zed as a Linux app under WSLg is **not supported**, and that you should run Zed on Windows and open WSL
  projects remotely. Workaround from that thread: `ZED_ALLOW_EMULATED_GPU=1` and `WAYLAND_DISPLAY=''`.
- [zed#47128](https://github.com/zed-industries/zed/issues/47128): GPUI panicked on WSLg's Wayland with
  `UnsupportedVersion`. GPUI required `xdg_wm_base` version 2 or higher, and WSLg's Weston 9 offers version 1. This was
  fixed by [zed#47185](https://github.com/zed-industries/zed/pull/47185) (commit `5eb2ff0`, 2026-01-23). Today the
  client binds `1..=6`
  ([`wayland/client.rs` L297-301](https://github.com/zed-industries/zed/blob/e7526b378acfa41cf6db0a89aacb31531b1fa087/crates/gpui_linux/src/linux/wayland/client.rs#L297-L301)).
  After the fix, the reporter ran Zed natively on WSLg Wayland with `GALLIUM_DRIVER=d3d12`. It still needed
  `ZED_ALLOW_EMULATED_GPU=1`, because Zed was on blade (Vulkan only) then, so it could only use lavapipe. The wgpu GL
  backend that came after is why the d3d12 GL path should now be picked.

## What we need to do

1. Put `export GALLIUM_DRIVER=d3d12` in the shell profile, or in a launcher or `cargo run` wrapper.
2. Install nothing else. `mesa-dri-drivers` (d3d12) and `mesa-vulkan-drivers` (lavapipe) are already there. Install
   `vulkan-tools` only if you want `vulkaninfo` for diagnosis.
3. Run once with `RUST_LOG=info` and check that the selected adapter is `D3D12 (...)` on the `Gl` backend.

If the app fails or renders wrong, try these in order:
- unset `GALLIUM_DRIVER` to get lavapipe in software, which is slow but correct;
- set `WAYLAND_DISPLAY=` to go through XWayland;
- `ZED_DEVICE_ID` cannot help here: GL adapters report device id 0, so the sort key never matches them.

## Not verified

- No GPUI binary was built or run here. The selection of the d3d12 GL adapter is worked out from the source code and
  the `glxinfo` result, not observed.
- `glxinfo` uses GLX on XWayland. GPUI on Wayland goes through **EGL**. `GALLIUM_DRIVER` is Mesa's Gallium loader
  switch and should apply to both, but EGL was not probed (no `eglinfo` installed).
- It is not confirmed that every GPUI shader path works on GLES 3.1 through d3d12.
- Zed still officially does not support WSLg. Any WSLg-specific bug in GPUI is ours to work around.
