# sysd - Minimal systemd-compatible init

A Rust implementation of a minimal init system that:
- Parses systemd .service unit files
- Provides D-Bus interface compatible with systemd-logind
- Manages cgroups v2 for process containment
- Runs as PID 1

## Goals

1. **Compatibility**: Parse Arch Linux systemd unit files (subset)
2. **Minimal**: Only implement features actually used
3. **Correct**: Proper PID 1 responsibilities (zombie reaping, signal handling)
4. **Interoperable**: Work with existing systemd components (logind, udevd, journald)

## Non-Goals

- Full systemd compatibility
- Socket activation (initially)
- Timers (use cron/fcron)
- User sessions (systemd --user)
- Generators
- Separate "jobs" queue (state shown inline on units)

## Directory Structure

sysd uses `/etc/sysd/` with symlinks to systemd files for compatibility:

```
/etc/sysd/
├── targets/
│   ├── default.target → /usr/lib/systemd/system/graphical.target
│   ├── graphical.target → /usr/lib/systemd/system/graphical.target
│   ├── graphical.target.wants → /etc/systemd/system/graphical.target.wants/
│   ├── multi-user.target → /usr/lib/systemd/system/multi-user.target
│   └── multi-user.target.wants → /etc/systemd/system/multi-user.target.wants/
└── system/                    # future: sysd-native service overrides
```

This allows:
- Reading existing systemd unit files without migration
- Admin can replace symlinks with custom files to override
- Gradual migration from systemd to sysd-native configs

## CLI Interface

Split architecture: `sysd` is the daemon, `sysdctl` is the CLI.

```
sysdctl list [--user]        # List units with state/PID
sysdctl status <service>     # Show service details
sysdctl start <service>      # Start a service
sysdctl stop <service>       # Stop a service
sysdctl restart <service>    # Restart a service
sysdctl enable <service>     # Enable service at boot
sysdctl disable <service>    # Disable service at boot
sysdctl is-enabled <service> # Check if enabled
sysdctl deps <service>       # Show dependencies
sysdctl get-boot-target      # Show default target
sysdctl reload-unit-files    # Reload unit files from disk
sysdctl sync-units           # Reload + restart changed
sysdctl parse <file>         # Debug: parse unit file (local)
sysdctl ping                 # Check daemon is running
```

Output example:
```
SERVICE                          STATE      PID     DESCRIPTION
docker.service                   running    1234    Docker Container Engine
nginx.service                    starting   -       Web Server
postgres.service                 inactive   -       PostgreSQL Database
```

States: `inactive`, `starting`, `running`, `stopping`, `failed`

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      PID 1 (sysd)                       │
├─────────────────────────────────────────────────────────┤
│  Signal Handler  │  Zombie Reaper  │  Shutdown Handler  │
├─────────────────────────────────────────────────────────┤
│                   Service Manager                        │
│  ┌─────────────┐ ┌─────────────┐ ┌───────────────────┐  │
│  │ Unit Parser │ │ Dep Resolver│ │ Process Supervisor│  │
│  └─────────────┘ └─────────────┘ └───────────────────┘  │
├─────────────────────────────────────────────────────────┤
│                    D-Bus Interface                       │
│  org.freedesktop.systemd1.Manager                       │
│  org.freedesktop.systemd1.Unit                          │
│  org.freedesktop.systemd1.Scope                         │
├─────────────────────────────────────────────────────────┤
│                   Cgroup Manager                         │
│  /sys/fs/cgroup/system.slice/                           │
│  /sys/fs/cgroup/user.slice/                             │
└─────────────────────────────────────────────────────────┘
```

## Components

### 1. PID 1 Core

When running as PID 1 (detected via `getpid() == 1`), sysd handles:

**Hardcoded early init** (like systemd):
- Mount /proc, /sys, /dev, /dev/pts, /dev/shm, /run, /sys/fs/cgroup
- Skips already-mounted filesystems (e.g., if initramfs handled them)

**Zombie reaping**:
- Polls `waitpid(-1, WNOHANG)` every 100ms
- Required: orphaned processes reparent to PID 1

**Signal handling**:
- SIGTERM → orderly poweroff
- SIGINT → orderly reboot
- SIGHUP → reload unit files (stub)
- SIGUSR1 → dump state to log
- SIGCHLD → triggers reap cycle

**Shutdown sequence**:
1. Stop all managed services
2. SIGTERM to all remaining processes
3. Wait 5s for graceful exit
4. SIGKILL to stragglers
5. sync() filesystems
6. Unmount all (except /, /proc, /sys, /dev)
7. reboot() syscall

### 2. Unit File Parser

Supported unit types:
- `.service` - Primary focus
- `.target` - Grouping/synchronization points
- `.scope` - Transient units for logind (created via D-Bus only)

#### Directive Support Matrix

Usage counts from `/usr/lib/systemd/system/*.service` on Arch Linux.

**[Unit] Section**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| Description= | 259 | ✓ done | Informational |
| Documentation= | 255 | ignore | URLs for man pages |
| After= | 205 | ✓ done | Ordering dependency |
| Before= | 197 | ✓ done | Reverse ordering |
| DefaultDependencies= | 146 | TODO | Usually `no` for early-boot units |
| Conflicts= | 126 | ✓ done | Stop these when starting |
| ConditionPathExists= | 82 | ✓ done | Skip if path missing |
| Wants= | 67 | ✓ done | Soft dependency |
| Requires= | 42 | ✓ done | Hard dependency |
| ConditionDirectoryNotEmpty= | 37 | TODO | Skip if dir empty |

**[Service] Section - Core**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| ExecStart= | 251 | ✓ done | Command to run |
| Type= | 213 | ✓ done | simple/forking/notify/dbus/oneshot/idle |
| RemainAfterExit= | 96 | ✓ done | For oneshot: stay "active" after exit |
| Restart= | 44 | ✓ done | no/on-failure/always |
| BusName= | 37 | ✓ done | Required for Type=dbus |
| ExecStop= | 25 | ✓ done | Stop command |
| TimeoutSec= | 24 | partial | Sets both start and stop timeout |
| RestartSec= | 23 | ✓ done | Delay before restart |
| KillMode= | 23 | ✓ done | control-group/process/mixed/none |
| User= | 22 | ✓ done | Run as user |
| ExecReload= | 16 | ✓ done | Reload command |
| ExecStartPre= | 10 | ✓ done | Pre-start commands |
| NotifyAccess= | 10 | TODO | Who can send sd_notify |

**[Service] Section - I/O**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| StandardInput= | 21 | TODO | null/tty/socket |
| StandardOutput= | 19 | ✓ done | journal/inherit/null |
| StandardError= | 15 | ✓ done | journal/inherit/null |
| TTYPath= | 9 | TODO | For getty-like services |
| TTYReset= | 9 | TODO | Reset TTY on start |

**[Service] Section - Environment**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| Environment= | 28 | ✓ done | KEY=value |
| EnvironmentFile= | 7 | ✓ done | Load from file |
| UnsetEnvironment= | 7 | TODO | Remove vars |
| WorkingDirectory= | ~20 | ✓ done | Chdir before exec |

**[Service] Section - Resource Limits**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| MemoryMax= | ~10 | ✓ done | Cgroup memory limit |
| CPUQuota= | ~5 | ✓ done | Cgroup CPU limit |
| TasksMax= | ~10 | ✓ done | Cgroup process limit |
| LimitNOFILE= | 15 | TODO | File descriptor limit |
| OOMScoreAdjust= | 12 | TODO | OOM killer priority |

**[Service] Section - Watchdog**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| WatchdogSec= | 29 | TODO | sd_notify watchdog timeout |

**[Service] Section - Security/Sandboxing** (can ignore initially)

| Directive | Count | Notes |
|-----------|-------|-------|
| DeviceAllow= | 64 | Cgroup device access |
| ImportCredential= | 62 | systemd credentials |
| SystemCallFilter= | 59 | seccomp |
| ProtectSystem= | 53 | read-only /, /usr |
| ProtectHome= | 51 | hide /home |
| NoNewPrivileges= | 47 | no setuid |
| CapabilityBoundingSet= | 42 | drop capabilities |
| ProtectKernelModules= | 37 | block module loading |
| PrivateTmp= | 36 | isolated /tmp |
| RestrictNamespaces= | 33 | block namespace creation |
| PrivateDevices= | 27 | isolated /dev |
| PrivateNetwork= | 20 | no network |
| ProtectProc= | 19 | /proc visibility |
| ReadWritePaths= | 15 | filesystem access |
| AmbientCapabilities= | 9 | grant capabilities |
| KeyringMode= | 5 | kernel keyring |

**[Install] Section**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| WantedBy= | 94 | ✓ done | Pulled by target |
| Also= | 25 | TODO | Enable related units |
| Alias= | 12 | TODO | Symlink name |
| DefaultInstance= | 2 | TODO | For templates |
| RequiredBy= | 1 | ✓ done | Required by target |

### 3. Dependency Resolver

- Build DAG from After/Before/Requires/Wants
- Topological sort for start order
- Detect cycles (error)
- Handle target units as synchronization points
- Parallel start where dependencies allow

### 4. Process Supervisor

Per-service state machine:
```
        ┌──────────┐
        │ inactive │
        └────┬─────┘
             │ start
        ┌────▼─────┐
        │ starting │──────────────┐
        └────┬─────┘              │ timeout/fail
             │ ready              │
        ┌────▼─────┐         ┌────▼────┐
        │ running  │         │ failed  │
        └────┬─────┘         └─────────┘
             │ stop/exit
        ┌────▼─────┐
        │ stopping │
        └────┬─────┘
             │ exited
        ┌────▼─────┐
        │ inactive │ (or restart)
        └──────────┘
```

### 5. sd_notify Protocol

Listen on `$NOTIFY_SOCKET` (unix datagram socket in /run):

| Message | Meaning |
|---------|---------|
| `READY=1` | Service startup complete |
| `RELOADING=1` | Service reloading config |
| `STOPPING=1` | Service shutting down |
| `STATUS=...` | Status string for display |
| `MAINPID=N` | Main PID changed |
| `WATCHDOG=1` | Watchdog keepalive |

Implementation: ~200 LOC

### 6. D-Bus Interface (for logind compatibility)

Bus name: `org.freedesktop.systemd1`
Object path: `/org/freedesktop/systemd1`

#### Manager Interface

Methods:
```
StartUnit(name: String, mode: String) -> ObjectPath
StopUnit(name: String, mode: String) -> ObjectPath
KillUnit(name: String, whom: String, signal: i32)
StartTransientUnit(name: String, mode: String, properties: Array) -> ObjectPath
Subscribe()
Reload()
```

Signals:
```
JobRemoved(id: u32, job: ObjectPath, unit: String, result: String)
UnitRemoved(unit: String, path: ObjectPath)
Reloading(active: bool)
```

Properties:
```
Version: String
```

#### Unit Interface

Properties:
```
Id: String
Description: String
ActiveState: String          # "active", "inactive", "failed", etc.
SubState: String             # "running", "dead", "exited", etc.
```

#### Scope Interface

Methods:
```
Abandon()
```

### 7. Cgroup Manager

Create and manage cgroup v2 hierarchy:

```
/sys/fs/cgroup/
├── system.slice/
│   ├── docker.service/
│   ├── NetworkManager.service/
│   └── ...
└── user.slice/
    └── user-1000.slice/
        ├── session-1.scope/    # Created by logind via D-Bus
        └── user@1000.service/
```

Operations:
- Create cgroup directories
- Write PIDs to cgroup.procs
- Configure controllers (memory, cpu, pids)
- Monitor cgroup.events for empty notification
- Clean up empty cgroups

## Crate Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full", "signal"] }
zbus = "5"                    # D-Bus
nix = { version = "0.29", features = ["signal", "process", "user", "fs", "mount", "reboot"] }
serde = { version = "1", features = ["derive"] }
thiserror = "2"               # Error types
log = "0.4"                   # Logging facade
env_logger = "0.11"           # Log output
clap = { version = "4", features = ["derive"] }  # CLI
shlex = "1"                   # Command parsing
libc = "0.2"                  # Low-level syscalls
```

## Milestones

### M1: Minimal Service Manager (no PID 1)
- [x] Parse .service files
- [x] Start/stop Type=simple services
- [x] Basic dependency ordering
- [x] CLI tool for testing (sysdctl)

### M2: sd_notify Support
- [x] NOTIFY_SOCKET listener
- [x] Type=notify services
- [x] READY/STOPPING handling

### M3: D-Bus Interface
- [x] org.freedesktop.systemd1.Manager
- [x] StartUnit/StopUnit/KillUnit
- [x] StartTransientUnit (stub - awaiting cgroups)
- [x] Signals (JobRemoved, UnitRemoved)

### M4: Cgroup Management
- [x] Create/remove cgroup directories
- [x] Move processes to cgroups
- [x] Resource limits (MemoryMax, CPUQuota, TasksMax)
- [x] Empty cgroup detection
- [x] Integrated with Manager (auto cgroup setup on start, cleanup on stop)

### M5: PID 1 Mode
- [x] Mount essential filesystems (hardcoded, same as systemd)
- [x] Zombie reaping (waitpid loop)
- [x] Signal handling (SIGTERM/SIGINT/SIGHUP/SIGUSR1)
- [x] Shutdown sequence (stop services → SIGTERM → SIGKILL → sync → unmount → reboot)
- [x] Run as init (kernel cmdline `init=/usr/bin/sysd`)

### M6: Service Types & Restart ✓
- [x] Restart= logic (on-failure, always) with RestartSec=
- [x] RemainAfterExit= for oneshot services
- [x] Type=forking (wait for parent exit, read PIDFile=)
- [x] KillMode= (control-group/process/mixed/none)
- [x] Type=idle (wait for job queue empty)
- [x] Type=dbus (watch BusName= on D-Bus)

### M7: Extended Features ✓
- [x] DefaultDependencies= (146 uses)
- [x] WatchdogSec= (29 uses)
- [x] Also= in [Install] (25 uses)
- [x] Alias= in [Install] (12 uses)
- [x] Template units (foo@.service) with %i/%I specifiers
- [x] Drop-in directories (.d/*.conf)
- [x] ConditionDirectoryNotEmpty=

### M8: Resource Limits
- [ ] LimitNOFILE= (file descriptors)
- [ ] OOMScoreAdjust=
- [ ] StandardInput=tty, TTYPath=, TTYReset= (for getty)

### Future: Security Sandboxing
Low priority - services run without sandboxing (like traditional init):
- ProtectSystem=, ProtectHome=, PrivateTmp=
- NoNewPrivileges=, CapabilityBoundingSet=
- SystemCallFilter= (seccomp)
- PrivateDevices=, PrivateNetwork=

## Testing Strategy

1. **Unit tests**: Parser, dependency resolver
2. **Integration tests**: Start/stop services in namespace
3. **VM tests**: Boot with sysd as PID 1 in QEMU
4. **Compatibility tests**: Run alongside real logind

## References

- [systemd.service(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
- [sd_notify(3)](https://www.freedesktop.org/software/systemd/man/latest/sd_notify.html)
- [org.freedesktop.systemd1 D-Bus API](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html)
- [cgroups v2 kernel docs](https://docs.kernel.org/admin-guide/cgroup-v2.html)
