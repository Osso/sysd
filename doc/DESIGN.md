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

```
sysd list [--user]     # List units with state/PID
sysd status <service>  # Show service details
sysd start <service>   # Start a service
sysd stop <service>    # Stop a service
sysd parse <file>      # Debug: show parsed unit file
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

Responsibilities:
- Mount essential filesystems (/proc, /sys, /dev, /run)
- Set up cgroup v2 hierarchy
- Reap zombie processes (wait4 loop)
- Handle signals (SIGTERM, SIGINT, SIGCHLD, SIGUSR1/2)
- Orderly shutdown sequence

### 2. Unit File Parser

Supported unit types:
- `.service` - Primary focus
- `.target` - Grouping/synchronization points
- `.scope` - Transient units for logind (created via D-Bus only)

Supported .service directives:

```ini
[Unit]
Description=             # String, informational
After=                   # Ordering (wait for these before starting)
Before=                  # Ordering (these wait for us)
Requires=                # Hard dependency (fail if dep fails)
Wants=                   # Soft dependency (don't fail if dep fails)
Conflicts=               # Stop these when starting us
ConditionPathExists=     # Skip if path doesn't exist

[Service]
Type=simple              # Fork/exec, ready immediately
Type=forking             # Fork/exec, ready when main exits
Type=notify              # Fork/exec, ready on sd_notify READY=1
Type=dbus                # Fork/exec, ready when D-Bus name acquired
Type=oneshot             # Run once, no main process

ExecStart=               # Command to run (required)
ExecStartPre=            # Commands before ExecStart
ExecStartPost=           # Commands after ExecStart
ExecStop=                # Command for stopping
ExecReload=              # Command for reloading

Restart=no               # Don't restart
Restart=on-failure       # Restart on non-zero exit
Restart=always           # Always restart

RestartSec=              # Delay before restart (default 100ms)
TimeoutStartSec=         # Timeout for startup
TimeoutStopSec=          # Timeout for stop (then SIGKILL)

User=                    # Run as user
Group=                   # Run as group
WorkingDirectory=        # Chdir before exec

Environment=             # KEY=value
EnvironmentFile=         # Path to env file

StandardOutput=journal   # Pipe stdout to journal
StandardError=journal    # Pipe stderr to journal

# Cgroup resource controls
MemoryMax=               # Memory limit
CPUQuota=                # CPU limit (percentage)
TasksMax=                # Max processes

[Install]
WantedBy=                # Target that pulls this in
RequiredBy=              # Target that requires this
```

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
zbus = "5"                    # D-Bus
tokio = { version = "1", features = ["full"] }
nix = "0.29"                  # Unix syscalls
serde = { version = "1", features = ["derive"] }
tracing = "0.1"               # Logging
tracing-subscriber = "0.3"

# Unit file parsing
configparser = "3"            # INI parser (or custom)
```

## Milestones

### M1: Minimal Service Manager (no PID 1)
- [ ] Parse .service files
- [ ] Start/stop Type=simple services
- [ ] Basic dependency ordering
- [ ] CLI tool for testing (sysdctl)

### M2: sd_notify Support
- [ ] NOTIFY_SOCKET listener
- [ ] Type=notify services
- [ ] READY/STOPPING handling

### M3: D-Bus Interface
- [ ] org.freedesktop.systemd1.Manager
- [ ] StartUnit/StopUnit/KillUnit
- [ ] StartTransientUnit (for logind)
- [ ] Signals (JobRemoved, UnitRemoved)

### M4: Cgroup Management
- [ ] Create/remove cgroup directories
- [ ] Move processes to cgroups
- [ ] Resource limits (MemoryMax, etc.)
- [ ] Empty cgroup detection

### M5: PID 1 Mode
- [ ] Mount essential filesystems
- [ ] Zombie reaping
- [ ] Signal handling
- [ ] Shutdown sequence
- [ ] Run as init (exec from initramfs)

### M6: Polish
- [ ] Type=dbus support
- [ ] Type=forking support
- [ ] Restart logic
- [ ] Drop-in directories
- [ ] Template units (foo@.service)

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
