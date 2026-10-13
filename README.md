# ro_launcher — a one-click launcher for the Ragnarok Offline Pre-Renewal Pack

**This repository is the launcher for the [Ragnarok Offline Pre-Renewal Pack](https://ragnarokoffline.github.io/)** — a complete
private-server build of Ragnarok Online: Laragon (PHP / Apache / Nginx / MariaDB),
a precompiled rAthena server, and a kRO-based client with the pre-renewal content.

The pack's own instructions say to launch it by hand:

> 1. **Start Database:** Run `laragon.exe` from the `1_Database` folder.
> 2. **Start Server:** Run `runserver.bat` from the `2_Server` folder. Keep the four command windows open.
> 3. **Start Game:** Run `Ragnarok.exe` from the `3_Client` folder.

`ro_launcher` does exactly that, in the right order, from a single window — and it
turns the stack off again when you close the game.

```
[ro_launcher]  Running laragon.exe
[ro_launcher]  laragon.exe Running (pid=13188) - MariaDB ready on 127.0.0.1:3306
[ro_launcher]  Starting all serv
[ro_launcher]  Running login-server.exe
[login-server.exe]  [Status]: The login-server is ready (Server is listening on the port 6900).
[char-server.exe]   [Status]: The char-server is ready (Server is listening on the port 6121).
[map-server.exe]    [Status]: Server is 'ready' and listening on port '5121'.
[ro_launcher]  Running Ragnarok.exe
[ro_launcher]  Ragnarok.exe Running (pid=1736)
```

One console, every program's output prefixed with its name, no leftover windows.

---

## Setup

### 1. Get the pack

Download `1_Database.7z`, `2_Server.7z` and `3_Client.7z` (plus `data.7z`,
`prerenewal.7z`, `rdata.7z` and a BGM pack) from the
[project's download page](https://ragnarokoffline.github.io/), and extract them
into a folder called `RO_PreRenewal`, e.g. `C:\RO_PreRenewal\`.

### 2. Put this repository next to them

```
RO_PreRenewal\
    1_Database\        <- from 1_Database.7z   (Laragon, MariaDB, …)
    2_Server\          <- from 2_Server.7z     (rAthena)
    3_Client\          <- from 3_Client.7z     (Ragnarok.exe, *.grf, BGM\)
    ro_launcher\       <- this repository (source)
    ro_launcher.exe    <- built by build.ps1
    ro_launcher.toml   <- configuration
```

Then build and play:

```powershell
cd ro_launcher
.\build.ps1
```

`build.ps1` produces `RO_PreRenewal\ro_launcher.exe`; double-click it (or run it
from a terminal) and approve the UAC prompt when it appears.

> **Tip from the pack's own notes:** create the account in your browser at
> <http://localhost> while the launcher is running. GM accounts are
> `admin` / `123456` and `admin2`.

---

## What it does

| Step | Program | Notes |
|---|---|---|
| 1 | `1_Database\laragon.exe` | GUI, its windows are hidden; waits for MariaDB on port 3306 |
| 2 | `2_Server\login-server.exe` | output captured as `[login-server.exe]`, gated on port 6900 |
| 2 | `2_Server\char-server.exe` | gated on port 6121 |
| 2 | `2_Server\web-server.exe` | gated on port 8888 |
| 2 | `2_Server\map-server.exe` | gated on port 5121 |
| 3 | `3_Client\Ragnarok.exe` | closing the game tears the whole stack down |

Every child process is started with `CREATE_NO_WINDOW`, so **no extra console
window is ever created**. Their stdout/stderr are read through pipes and
re-printed into the launcher console, prefixed with the program name.

### Console commands

| Key | Action |
|---|---|
| `s` | status table (process, state, pid, restart policy) |
| `r <key>` | restart a server now (`login`, `char`, `web`, `map`) |
| `q` | graceful shutdown (same path as closing the game) |
| `Ctrl+C` | graceful shutdown |

---

## What was ported from the batch scripts

`2_Server\runserver.bat` (default target `watch`) is reimplemented in
`src/server.rs`, keeping its behaviour:

* `:getXStatus` used `tasklist /FI "IMAGENAME eq <exe>"` — now a process-table
  lookup. If an instance is already running it logs
  `Login-Server serv is already running, pid=…` instead of starting a second one.
* `serv.bat` ran the executable and then looped forever, restarting 15 seconds
  after a crash. That loop is now a supervisor thread per server, logging
  `Map-Server has crashed!` / `Restarting in 15 seconds, press Ctrl+C to cancel.`
  and re-spawning. `restart = "off"` gives you the `runserver.bat start`
  behaviour instead.

### Deliberate improvements

* **Ports instead of races.** `runserver.bat` started all four servers at once,
  so they raced on the database and on each other's ports. The launcher starts
  them in order and waits for each port to open.
* **Nothing is orphaned.** Everything runs inside a Windows Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so MariaDB, nginx and Apache (which
  Laragon starts itself) die with the launcher even if it is killed from Task
  Manager.
* **Smart termination.** Processes are matched on `(pid, creation time)`: an app
  that already closed is skipped silently, and a recycled PID is never killed.
* **Clean database shutdown.** MariaDB is stopped with
  `mariadb-admin shutdown`. A hard kill marks MyISAM tables
  (`ragnarok.login`, `vendings`, `buyingstores`, …) as *crashed and should be
  repaired*.

---

## Portability

The folder can be moved anywhere; everything is resolved relative to the project
root (the directory holding `1_Database`, `2_Server` and `3_Client`).

* **Stale path repair (always on).** Laragon and `my.ini` store absolute paths.
  After a move they still point at the old location and MariaDB refuses to
  start, so on every launch the launcher rewrites them, keeping a `.bak` copy of
  each file it touches. There is no switch to turn this off.
* **Spaces in the path.** The pack's own notes recommend avoiding spaces, and
  Laragon refuses them outright ("Laragon path must not have SPACE") because it
  drives its services through `cmd.exe`. When the root contains a space, the
  launcher creates a space-free junction (`D:\RO_Link` → project root) and
  starts Laragon through it. If the junction cannot be created it starts
  `mysqld.exe` directly instead — same MariaDB, same `my.ini`, and its console
  output is captured too.
* **Elevation.** `Ragnarok.exe` is marked *requireAdministrator*, so the
  launcher re-launches itself once through UAC.
* **Client registry.** `Ragnarok.exe` will not open its window unless
  `HKCU\Software\gravity\RagnarokOnline` points at the install (normally written
  by `opensetup.exe`). The launcher writes those path values itself, so a fresh
  copy works without any manual setup.

---

## Configuration

`ro_launcher.toml` is **optional** — the launcher ships with built-in defaults
that match the layout above, so deleting the file changes nothing. It exists to
*override* things:

```toml
[laragon]
mode = "auto"            # "auto" | "laragon" | "direct" (mysqld without Laragon)
link_path = 'D:\RO_Link'

[[servers]]
key = "map"
exe = "map-server.exe"
ready_port = 5121
restart = "on"           # "on" = 15s crash restart, "off" = stay down

restart_delay_secs = 15
log_to_file = true       # tees the console to ro_launcher.log
```

All paths are relative to the project root; absolute paths are also accepted.
The copy in this repository is the template — `build.ps1` refreshes the copy in
the project root from it, so edit **this** file, not the shipped one.

Command line:

```
ro_launcher.exe [OPTIONS]
  --root <DIR>      override the project root (or set RO_ROOT)
  --dry-run         validate the configuration and print the boot plan only
  --no-elevate      never re-launch through UAC
  --no-color        plain text
  --timestamps      prefix each line with HH:MM:SS.mmm
  --no-log-file     do not write ro_launcher.log
  --no-tail         do not tail MariaDB's .err file
```

---

## Building

Requirements: the MSVC toolchain (`rustup default stable-x86_64-pc-windows-msvc`)
and Visual Studio Build Tools (for `link.exe`).

### Release build (publishes to the project root)

```powershell
cd ro_launcher
.\build.ps1
```

This runs `cargo build --release` and copies the binary, the configuration and
this README next to `1_Database`.

### Development build

```powershell
cd ro_launcher
cargo build       # target\debug\ro_launcher.exe, runnable straight from there
cargo test        # unit tests
cargo clippy
```

The project root is auto-detected from the executable's location
(`ro_launcher\target\debug\ro_launcher.exe` → three levels up), so a debug build
finds `1_Database` and friends without any arguments.

---

## Repository layout

Only the launcher source is tracked here; `1_Database`, `2_Server` and
`3_Client` are multi-gigabyte game data that sit **outside** the repository, so
they cannot be committed by accident.

```
ro_launcher/
  Cargo.toml / build.rs     build.rs embeds app.ico + manifest
  build.ps1                 release build, publishes to the project root
  ro_launcher.toml          configuration template
  src/
    main.rs        boot sequence, preflight, shutdown orchestration
    config.rs      ro_launcher.toml + project-root discovery
    logging.rs     the single console writer (one thread owns stdout)
    proc.rs        spawn with CREATE_NO_WINDOW, pipe capture, smart terminate
    job.rs         Windows Job Object (kill-on-close)
    database.rs    step 1: Laragon or mysqld, plus graceful DB shutdown
    server.rs      step 2: runserver.bat / serv.bat reimplemented
    client.rs      step 3: Ragnarok.exe and the session wait
    clientreg.rs   HKCU\...\gravity\RagnarokOnline bootstrap
    portable.rs    stale-path repair + space-free junction
    gate.rs        TCP readiness polling
    registry.rs    tracked processes, cancelable sleeps, state
    mariadb.rs     tails MariaDB's .err as a fifth prefixed source
    elevate.rs     UAC self-elevation
```

---

## Credits

The pack itself (server, client, database, custom content) is the work of the
[Ragnarok Offline Pre-Renewal Pack](https://ragnarokoffline.github.io/) authors.
This repository only automates the launch procedure described there and does not
redistribute any of its files.