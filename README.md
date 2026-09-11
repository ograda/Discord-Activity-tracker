# Discord Activity MVP

A small, stateless Windows process watcher written in Rust. It checks a short
XML-defined list of processes and combines every detected match into one
Discord Rich Presence.

It intentionally has no history, database, server, account, UI, telemetry, or
automatic discovery.

## Discord output

With `Wow.exe`, `Code.exe`, and `obs64.exe` running, the presence is:

```text
Active: World of Warcraft
Also: Visual Studio Code + OBS Studio
```

The first detected activity in XML order becomes the primary activity. All
other detected activities are combined into the secondary line.

## Requirements

- Windows 10 or 11, 64-bit
- Current stable Rust toolchain using the MSVC target
- Discord desktop client
- A Discord application ID

## Install Rust on Windows

1. Download and run `rustup-init.exe` from <https://rustup.rs/>.
2. Choose the default installation when prompted.
3. If requested, install **Visual Studio Build Tools** with the **Desktop
   development with C++** workload. Rust uses Microsoft's linker on Windows.
4. Close and reopen PowerShell.
5. Verify the installation:

```powershell
rustc --version
cargo --version
rustup show active-toolchain
```

The active toolchain should end in `x86_64-pc-windows-msvc`.

## Configure Discord and activities

1. Create an application at <https://discord.com/developers/applications>.
2. Copy its **Application ID** from **General Information**.
3. Open PowerShell in the project directory.
4. Create the working configuration:

```powershell
Copy-Item .\activities.example.xml .\activities.xml
notepad .\activities.xml
```

5. Replace the example `discord-application-id`.
6. Replace or reorder the activities and executable names.

Each activity may have several executable aliases:

```xml
<activity>
    <name>My Game</name>
    <windows-process>MyGame.exe</windows-process>
    <windows-process>MyGameLauncher.exe</windows-process>
</activity>
```

Process matching is exact but case-insensitive after surrounding whitespace is
removed. Task Manager's **Details** tab can be used to find an executable's
actual process name.

## Compile on Windows

Run the tests first:

```powershell
cargo test
```

Compile the optimized executable:

```powershell
cargo build --release
```

The executable is created at:

```text
target\release\discord-activity-mvp.exe
```

The included Cargo configuration statically links the Microsoft C runtime for
a more self-contained release executable.

## Run

Keep Discord desktop open, then run:

```powershell
.\target\release\discord-activity-mvp.exe .\activities.xml
```

Keep the PowerShell window open while tracking. Stop with `Ctrl+C`; the tracker
clears its Discord presence before closing.

For a development run that compiles and starts immediately:

```powershell
cargo run --release -- .\activities.xml
```

If no configuration argument is passed, the program looks for
`activities.xml` in its current working directory.

## Compile-time Windows selection

Rust automatically defines target configuration values. This project currently
uses the following guard in `src/main.rs`:

```rust
#[cfg(not(windows))]
compile_error!("discord-activity-mvp currently supports Windows only");
```

This is the Rust equivalent of checking `_WIN32` or `_WIN64` in C++. The target
is selected during compilation, so this adds no runtime platform check.

When Linux support is implemented, platform-specific modules can be selected
like this:

```rust
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;
```

## GitHub executable build

The repository includes a **Build binaries** workflow. After pushing it to
GitHub, open **Actions → Build binaries → Run workflow**. Download the
`discord-activity-mvp-windows-x64` artifact when the workflow finishes. It
contains the Windows `.exe`, sample XML, and this README.

The build also runs automatically for tags beginning with `v`, such as
`v0.1.0`.

## Behavior

- Processes are refreshed at the configured polling interval.
- Discord is updated only when the detected activity set changes.
- The current presence is resent every 60 seconds so restarting Discord does
  not require restarting this tracker.
- If Discord is closed, connection attempts continue without terminating the
  tracker.
- Session elapsed time exists only in memory and resets when the detected set
  changes. Nothing is persisted.

## License

MIT
