# One binary that is both a console CLI/TUI and a GUI on Windows

Research for [#13](https://github.com/EwenSellitto/Commandant/issues/13). The
client binary `commandant` holds a clap CLI, a ratatui/crossterm TUI and a GPUI
GUI (`commandant gui`), and must run on Windows and Linux. Researched 2026-10-06.

## Short answer

Keep `commandant` a **console-subsystem** exe (the Rust default) so the CLI and
TUI behave like any terminal program. Make `commandant gui` relaunch itself with
`DETACHED_PROCESS` and exit, so the shell gets its prompt back. Embed a manifest
with `consoleAllocationPolicy = detached`, so on Windows 11 24H2+ a shortcut that
runs `commandant.exe gui` opens no console window at all. On older Windows that
shortcut flashes a console briefly before the relaunch. If that flash matters,
add a second, tiny windows-subsystem bin (`commandant-gui.exe`), which is the
WezTerm pattern.

Do **not** use `windows_subsystem = "windows"` + `AttachConsole` for this
binary: the shell does not wait for a GUI-subsystem process, so the TUI would
share the console with a live shell prompt.

## The underlying facts

- Each PE exe is marked with one subsystem, fixed at link time. Rust sets it with
  `#![windows_subsystem = "console" | "windows"]` on the crate root. `"console"`
  is the default. The attribute is ignored on non-Windows targets.
  [Rust reference][rust-ref]
- A console (CUI) process attaches to its parent's console, or gets a **new
  console window** if the parent has none (for example, when launched from
  Explorer or a shortcut). A windows (GUI) process "will run detached from any
  existing console". [Rust reference][rust-ref], [/SUBSYSTEM][subsystem]
- **Shells decide whether to wait from the subsystem.** "executing such an
  application [CUI] inside a shell like CMD or PowerShell will block until the
  application has finished executing. Neither of these are true for
  IMAGE_SUBSYSTEM_WINDOWS_GUI applications. It'll neither be allocated a console,
  nor block execution inside a shell." [Console Allocation Policy][cap]. The
  terminal team's spec adds: "The decision to pause/wait is made entirely in the
  calling shell, and the console subsystem cannot influence that decision."
  [spec #7335][spec]
- Raymond Chen's verdict on "both at once": "You can't, but you can try to fake
  it." The console is set up before the program's code runs. [Old New Thing][chen]

## Options

### A. Windows subsystem + `AttachConsole(ATTACH_PARENT_PROCESS)`

Link as GUI, then attach to the parent's console at startup when there is one.
`AttachConsole` "is primarily useful to applications that were linked with
/SUBSYSTEM:WINDOWS"; until it is called, `GetStdHandle` handles "will likely be
invalid ... The exception to this is if the application is launched with handle
inheritance by its parent process" (so redirected/piped output still works).
It fails with `ERROR_INVALID_HANDLE` when the parent has no console.
[AttachConsole][attach]

Used by: Alacritty (unconditionally, plus `FreeConsole` at exit: "Without
explicitly detaching the console cmd won't redraw it's prompt") [alacritty
main.rs][alacritty]; Zed (release builds, only with `--foreground`) [zed
main.rs][zed]; Neovide (release builds) [neovide][neovide]; WezTerm's
`wezterm-gui.exe` (only with `--attach-parent-console`) [wezterm-gui
main.rs][wezterm-gui].

- Good: no console window from Explorer, on every Windows version. One binary.
- Bad: the shell does not wait. Output lands after the prompt is already drawn,
  and the prompt is not redrawn when the program exits. WezTerm's own comment:
  "we will be running asynchronously from the shell in the command window, which
  means that it will appear to the user that we hung at the end, when in reality
  the shell is waiting for input but didn't know to re-draw the prompt."
  [wezterm-gui main.rs][wezterm-gui]. The terminal spec says such apps "end up
  stomping on the output of any shell that doesn't wait for them". [spec][spec]
- **Bad for the TUI (inference):** since the shell is not waiting, the shell and
  the TUI are both attached to the same console and both reading its input at
  once. Raw mode, the alternate screen and key events would fight a live prompt.
  None of the projects above runs a full-screen TUI this way; they only print
  logs or `--help`. Exit codes are also lost to an interactive shell that has
  already moved on.
- Rust detail: std calls `GetStdHandle` on every access and treats
  `ERROR_INVALID_HANDLE` as a closed stream, so `println!` without a console does
  not panic and starts working after `AttachConsole`. [std stdio/windows.rs][std]

Verdict: fine for a GUI that sometimes prints. Wrong for a binary whose main job
is a CLI and a TUI.

### B. Console subsystem, hide or drop the console when launched from Explorer

Stay CUI. In GUI mode, if this process is the only one on its console
(`GetConsoleProcessList` returns 1, so the console was created for us), call
`FreeConsole`; "A console is closed when the last process attached to it
terminates or calls FreeConsole". [FreeConsole][free],
[GetConsoleProcessList][gcpl]

- Good: CLI/TUI are perfect: the shell waits, input is not shared, and exit
  codes and pipes work.
- Bad: from Explorer, a console window is created and then destroyed, so it
  **flashes**. `GetConsoleProcessList` is marked "not recommended" by Microsoft
  (no VT equivalent). [GetConsoleProcessList][gcpl]
- `commandant gui` run from a terminal blocks the shell until the window closes,
  unless the GUI relaunches itself detached (see D).

### C. Two executables: console `commandant.exe` + windows `commandant-gui.exe`

The console exe handles CLI/TUI and spawns the GUI exe for `gui`. Shortcuts point
at the GUI exe.

Used by: WezTerm: "`wezterm.exe` ... for interacting with wezterm from the
terminal", "`wezterm-gui.exe` ... for spawning wezterm from a desktop
environment"; launchers should point at the GUI binary "so that Windows itself
doesn't pop up a console host"; `wezterm-gui.exe --help` prints nothing.
[WezTerm CLI docs][wezterm-docs]. Zed ships a separate CLI `zed.exe` in `bin/`
next to the GUI `zed.exe` (the `cli` crate). [zed cli crate][zed-cli]. The
terminal spec names this "ship two binaries identical except for the subsystem"
pattern (`python`/`pythonw`) as the status quo it wanted to replace. [spec][spec]

- Good: each exe is correct in its own environment, on every Windows version, no
  hacks.
- Bad: two files to ship and keep together. In Cargo this costs little: a second
  `[[bin]]` whose `main.rs` sets `#![windows_subsystem = "windows"]` and calls
  the same library entry point (each bin is its own crate root). On Linux the
  second bin is redundant; it can be built for Windows only.

### D. Console subsystem + `consoleAllocationPolicy = detached` manifest (Windows 11 24H2+)

Microsoft's official answer to this exact problem. Build as CUI and embed:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application>
    <windowsSettings>
      <consoleAllocationPolicy xmlns="http://schemas.microsoft.com/SMI/2024/WindowsSettings">detached</consoleAllocationPolicy>
    </windowsSettings>
  </application>
</assembly>
```

"The IMAGE_SUBSYSTEM_WINDOWS_CUI type informs shells that they need to block
until your application has finished executing, while the application manifest
informs the operating system to skip allocating a console."
[Console Allocation Policy][cap]. With `detached`, the process gets a console
only if it inherits one; launched from Explorer it gets none, and "All console
inheritance will proceed as normal". [spec][spec], [application
manifests][manifests]. Supported on Windows 11 24H2 (build 26100) / Server 2025
and later; older versions ignore the element and behave as plain CUI.
[manifests][manifests]. Companion API `AllocConsoleWithOptions` (same minimum
version) creates a console on demand. [AllocConsoleWithOptions][acwo]

- Good: one exe. CLI/TUI behave exactly like a normal console program. No console
  window from Explorer/shortcuts.
- Bad: the shell still waits for the exe, so `commandant gui` from a terminal
  blocks the prompt until the window closes ("will cause the shell to 'hang'").
  [spec][spec]. Fix: `gui` relaunches itself with `DETACHED_PROCESS` ("the new
  process does not inherit its parent's console") and exits.
  [process creation flags][pcf]
- Bad: pre-24H2 Windows 10/11 gets a console window from a shortcut. With the
  relaunch, it only lives until the launcher exits (a flash, as in B).
- Embedding the manifest: MSVC link args `/MANIFEST:EMBED /MANIFESTINPUT:<file>`
  ([/MANIFESTINPUT][mi]) passed from `build.rs` via
  `cargo:rustc-link-arg-bins=`, only when `CARGO_CFG_TARGET_OS == "windows"`
  (and the MSVC env). For the GNU target a resource compiler crate is needed
  instead. Not verified against a real build here.

### E. `.com` / `.exe` pair (devenv style)

Same as C, but name the console exe `commandant.com` and the GUI one
`commandant.exe`. Typing `commandant` in cmd resolves `.com` first because
PATHEXT lists `.COM;.EXE;...` in that order ([start / PATHEXT][pathext]);
Explorer and shortcuts use the `.exe`. Visual Studio does exactly this:
"Commands that begin with `devenv` are handled by the `devenv.com` utility,
which delivers output through standard system streams ... Using `devenv.exe`
directly prevents output from appearing on the console." [devenv docs][devenv]

- Good: one command name for users.
- Bad: everything in C, plus an unusual file name that Cargo does not produce
  (rename at packaging time), and resolution depends on PATHEXT order (not
  checked for PowerShell or other shells here). Little gain over C for a project
  whose binary is mostly a CLI.

## Things that apply to every option

- **Child processes of a console-less GUI.** A console-subsystem child of a
  process with no console gets a new console window (console apps "are
  initialized with a console, unless they are created as detached processes").
  [AllocConsole][alloc]. Any `git`/`ssh`/shell the GUI spawns should use
  `CREATE_NO_WINDOW` through `std::os::windows::process::CommandExt::creation_flags`.
  [process creation flags][pcf]
- **Linux:** none of this applies. `windows_subsystem` is ignored off Windows
  [rust-ref], and the manifest/`DETACHED_PROCESS` code sits behind
  `#[cfg(windows)]`. If the shell should also return on Linux, `gui` can spawn
  itself and exit there too (Neovide forks for the same reason
  [neovide main.rs][neovide-main]).

## Comparison

| | CLI/TUI in a terminal | Shortcut / Explorer | `commandant gui` from a shell | Files | Windows versions |
|---|---|---|---|---|---|
| A. GUI + AttachConsole | broken TUI, prompt races output | clean | returns at once | 1 | all |
| B. CUI + FreeConsole | correct | console flash | blocks unless relaunched | 1 | all |
| C. two exes | correct | clean (GUI exe) | returns (spawns GUI exe) | 2 | all |
| D. CUI + manifest + relaunch | correct | clean on 24H2+, flash before | returns (relaunch) | 1 | best on 24H2+ |
| E. .com/.exe | correct | clean | returns | 2 | all |

## Recommendation

D: console subsystem, `detached` manifest, and `gui` relaunches itself with
`DETACHED_PROCESS`. It keeps one binary and keeps the CLI/TUI as a normal console
program (the part that has to work perfectly). It costs one manifest file, a few
lines in `build.rs`, and about ten lines for the relaunch. The only gap is a
brief console flash from shortcuts on pre-24H2 Windows. If that turns out to
matter, move to C by adding a second `[[bin]]` without touching the rest.

## Sources

[rust-ref]: https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute
[subsystem]: https://learn.microsoft.com/en-us/cpp/build/reference/subsystem-specify-subsystem
[cap]: https://learn.microsoft.com/en-us/windows/console/console-allocation-policy
[spec]: https://github.com/microsoft/terminal/blob/main/doc/specs/%237335%20-%20Console%20Allocation%20Policy.md
[manifests]: https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests#consoleallocationpolicy
[acwo]: https://learn.microsoft.com/en-us/windows/console/allocconsolewithoptions
[attach]: https://learn.microsoft.com/en-us/windows/console/attachconsole
[alloc]: https://learn.microsoft.com/en-us/windows/console/allocconsole
[free]: https://learn.microsoft.com/en-us/windows/console/freeconsole
[gcpl]: https://learn.microsoft.com/en-us/windows/console/getconsoleprocesslist
[pcf]: https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags
[mi]: https://learn.microsoft.com/en-us/cpp/build/reference/manifestinput-specify-manifest-input
[devenv]: https://learn.microsoft.com/en-us/visualstudio/ide/reference/devenv-command-line-switches
[pathext]: https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-xp/bb491005(v=technet.10)
[chen]: https://devblogs.microsoft.com/oldnewthing/20090101-00/?p=19643
[alacritty]: https://github.com/alacritty/alacritty/blob/master/alacritty/src/main.rs
[zed]: https://github.com/zed-industries/zed/blob/main/crates/zed/src/main.rs
[zed-cli]: https://github.com/zed-industries/zed/tree/main/crates/cli
[neovide]: https://github.com/neovide/neovide/blob/main/src/windows_utils.rs
[neovide-main]: https://github.com/neovide/neovide/blob/main/src/main.rs
[wezterm-gui]: https://github.com/wezterm/wezterm/blob/main/wezterm-gui/src/main.rs
[wezterm-docs]: https://wezterm.org/cli/general.html
[std]: https://github.com/rust-lang/rust/blob/master/library/std/src/sys/stdio/windows.rs

- Rust reference, `windows_subsystem`: <https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute>
- Microsoft, Console Allocation Policy: <https://learn.microsoft.com/en-us/windows/console/console-allocation-policy>
- Microsoft, application manifests (`consoleAllocationPolicy`): <https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests#consoleallocationpolicy>
- microsoft/terminal spec #7335, Console Allocation Policy: <https://github.com/microsoft/terminal/blob/main/doc/specs/%237335%20-%20Console%20Allocation%20Policy.md>
- Microsoft, AttachConsole / AllocConsole / AllocConsoleWithOptions / FreeConsole / GetConsoleProcessList: <https://learn.microsoft.com/en-us/windows/console/attachconsole>, <https://learn.microsoft.com/en-us/windows/console/allocconsole>, <https://learn.microsoft.com/en-us/windows/console/allocconsolewithoptions>, <https://learn.microsoft.com/en-us/windows/console/freeconsole>, <https://learn.microsoft.com/en-us/windows/console/getconsoleprocesslist>
- Microsoft, /SUBSYSTEM, /MANIFESTINPUT, process creation flags: <https://learn.microsoft.com/en-us/cpp/build/reference/subsystem-specify-subsystem>, <https://learn.microsoft.com/en-us/cpp/build/reference/manifestinput-specify-manifest-input>, <https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags>
- Microsoft, devenv command-line switches: <https://learn.microsoft.com/en-us/visualstudio/ide/reference/devenv-command-line-switches>
- Microsoft, `start` (PATHEXT order): <https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-xp/bb491005(v=technet.10)>
- Raymond Chen, "How do I write a program that can be run either as a console or a GUI application?": <https://devblogs.microsoft.com/oldnewthing/20090101-00/?p=19643>
- Source: Alacritty, Zed (app and cli crate), Neovide, WezTerm (`wezterm-gui`), Rust std `stdio/windows.rs`, and WezTerm CLI docs, linked above.
