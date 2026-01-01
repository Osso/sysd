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
- User sessions (systemd --user) - future M13
- Generators - future, low priority
- Separate "jobs" queue (state shown inline on units)

## Boot Requirements

For a minimal graphical boot, the following are critical:

1. **Socket activation (M10)** - dbus.socket is required by most services
2. **Basic targets** - multi-user.target → graphical.target chain ✓
3. **Service management** - start/stop/restart ✓
4. **D-Bus interface** - for logind compatibility ✓
5. **Cgroups** - process containment ✓

Current blockers for real boot:
- No socket activation → dbus-broker won't start → most services fail

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
sysdctl list [--user]           # List units with state/PID
sysdctl status <service>        # Show service details
sysdctl start <service>         # Start a service
sysdctl stop <service>          # Stop a service
sysdctl restart <service>       # Restart a service
sysdctl enable <service>        # Enable service at boot
sysdctl disable <service>       # Disable service at boot
sysdctl is-enabled <service>    # Check if enabled
sysdctl deps <service>          # Show dependencies
sysdctl get-boot-target         # Show default target
sysdctl reload                  # Reload unit files from disk
sysdctl sync                    # Reload + restart changed services
sysdctl switch-target <target>  # Switch to target, stop unrelated units
sysdctl parse <file>            # Debug: parse unit file (local)
sysdctl ping                    # Check daemon is running
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
- SIGHUP → reload unit files
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
| DefaultDependencies= | 146 | ✓ done | Usually `no` for early-boot units |
| Conflicts= | 126 | ✓ done | Stop these when starting |
| ConditionPathExists= | 82 | ✓ done | Skip if path missing |
| Wants= | 67 | ✓ done | Soft dependency |
| Requires= | 42 | ✓ done | Hard dependency |
| ConditionDirectoryNotEmpty= | 37 | ✓ done | Skip if dir empty |

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
| NotifyAccess= | 10 | DONE | M19: validate_notify_access() |

**[Service] Section - I/O**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| StandardInput= | 21 | ✓ done | null/tty/socket |
| StandardOutput= | 19 | ✓ done | journal/inherit/null |
| StandardError= | 15 | ✓ done | journal/inherit/null |
| TTYPath= | 9 | ✓ done | For getty-like services |
| TTYReset= | 9 | ✓ done | Reset TTY on start |

**[Service] Section - Environment**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| Environment= | 28 | ✓ done | KEY=value |
| EnvironmentFile= | 7 | ✓ done | Load from file |
| UnsetEnvironment= | 7 | ✓ done | Remove vars via env_remove() |
| WorkingDirectory= | ~20 | ✓ done | Chdir before exec |

**[Service] Section - Resource Limits**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| MemoryMax= | ~10 | ✓ done | Cgroup memory limit |
| CPUQuota= | ~5 | ✓ done | Cgroup CPU limit |
| TasksMax= | ~10 | ✓ done | Cgroup process limit |
| LimitNOFILE= | 15 | ✓ done | File descriptor limit |
| OOMScoreAdjust= | 12 | ✓ done | OOM killer priority |

**[Service] Section - Watchdog**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| WatchdogSec= | 29 | ✓ done | sd_notify watchdog timeout |

**[Service] Section - Security/Sandboxing**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| DevicePolicy= | 64 | ✓ done | Mount namespace device isolation |
| DeviceAllow= | 64 | ✓ done | Bind mount devices with r/rw perms |
| ImportCredential= | 62 | ignore | systemd credentials |
| SystemCallFilter= | 59 | partial | seccomp (parsed, not enforced) |
| ProtectSystem= | 53 | ✓ done | read-only /, /usr |
| ProtectHome= | 51 | ✓ done | hide /home |
| NoNewPrivileges= | 47 | ✓ done | no setuid |
| CapabilityBoundingSet= | 42 | ✓ done | drop capabilities |
| ProtectKernelModules= | 37 | ✓ done | block module loading |
| PrivateTmp= | 36 | ✓ done | isolated /tmp |
| RestrictNamespaces= | 33 | partial | parsed, not enforced |
| PrivateDevices= | 27 | ✓ done | isolated /dev |
| PrivateNetwork= | 20 | ✓ done | no network |
| ProtectProc= | 19 | ✓ done | /proc visibility |
| ReadWritePaths= | 15 | ✓ done | filesystem access |
| AmbientCapabilities= | 9 | ✓ done | grant capabilities |
| KeyringMode= | 5 | ignore | kernel keyring |

**[Install] Section**

| Directive | Count | Status | Notes |
|-----------|-------|--------|-------|
| WantedBy= | 94 | ✓ done | Pulled by target |
| Also= | 25 | ✓ done | Enable related units |
| Alias= | 12 | ✓ done | Symlink name |
| DefaultInstance= | 2 | DONE | M19: Template loading |
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
- [x] Type=notify services (39 services)
- [x] READY/STOPPING handling

### M3: D-Bus Interface
- [x] org.freedesktop.systemd1.Manager
- [x] StartUnit (logind: start user@.service on first login)
- [x] StopUnit (logind: stop user service on last logout)
- [x] KillUnit (logind: kill session scope on logout)
- [x] StartTransientUnit (M14; logind: create session-N.scope with cgroups)
- [x] Subscribe (logind: watch for signals)
- [x] Signals: JobRemoved (logind: confirm scope/service started), UnitRemoved

### M4: Cgroup Management
- [x] Create/remove cgroup directories
- [x] Move processes to cgroups
- [x] Resource limits: MemoryMax= (1 use), CPUQuota= (0 uses), TasksMax= (6 uses)
- [x] Empty cgroup detection
- [x] Integrated with Manager (auto cgroup setup on start, cleanup on stop)

### M5: PID 1 Mode
- [x] Mount essential filesystems (hardcoded, same as systemd)
- [x] Zombie reaping (waitpid loop)
- [x] Signal handling (SIGTERM/SIGINT/SIGHUP/SIGUSR1)
- [x] Shutdown sequence (stop services → SIGTERM → SIGKILL → sync → unmount → reboot)
- [x] Run as init (kernel cmdline `init=/usr/bin/sysd`)

### M6: Service Types & Restart ✓
- [x] Restart= logic (on-failure, always) with RestartSec= (44 uses)
- [x] RemainAfterExit= for oneshot services (96 uses)
- [x] Type=forking (wait for parent exit, read PIDFile=) (5 uses)
- [x] KillMode= (control-group/process/mixed/none) (23 uses)
- [x] Type=idle (wait for job queue empty) (7 uses)
- [x] Type=dbus (watch BusName= on D-Bus) (16 uses)

### M7: Extended Features ✓
- [x] DefaultDependencies= (146 uses)
- [x] WatchdogSec= (29 uses)
- [x] Also= in [Install] (25 uses)
- [x] Alias= in [Install] (12 uses)
- [x] Template units (foo@.service) with %i/%I specifiers (52 templates)
- [x] Drop-in directories (.d/*.conf) (5 dirs)
- [x] ConditionDirectoryNotEmpty= (37 uses)

### M8: Resource Limits ✓
- [x] LimitNOFILE= (15 uses)
- [x] OOMScoreAdjust= (12 uses)
- [x] StandardInput=tty (15 uses), TTYPath= (6 uses), TTYReset= (9 uses)

### M9: Security Sandboxing ✓
- [x] NoNewPrivileges= (47 uses) - prctl(PR_SET_NO_NEW_PRIVS)
- [x] ProtectSystem= (53 uses) - read-only bind mounts for /, /usr
- [x] ProtectHome= (51 uses) - hide/read-only /home, /root, /run/user
- [x] PrivateTmp= (36 uses) - isolated /tmp and /var/tmp
- [x] CapabilityBoundingSet= (42 uses) - drop capabilities
- [x] AmbientCapabilities= (9 uses) - grant capabilities
- [x] PrivateDevices= (27 uses) - isolated /dev with only null/zero/full/random/urandom
- [x] PrivateNetwork= (20 uses) - isolated network namespace
- [x] RestrictNamespaces= (33 uses) - block namespace creation (parsed, not enforced)
- [x] ProtectKernelModules= (37 uses) - block module loading
- [x] ProtectProc= (19 uses) - /proc visibility restrictions
- [x] ReadWritePaths=/ReadOnlyPaths= (15 uses) - filesystem access control
- [x] SystemCallFilter= (59 uses) - seccomp filtering (parsed, not enforced)

### M10: Socket Activation ✓
Critical for boot - dbus.socket must work for most services.
- [x] Parse .socket unit files (54 units; ListenStream= 45, ListenDatagram= 4, Accept= 15)
- [x] Create listening sockets (Unix stream/dgram, TCP, UDP, FIFO)
- [x] Pass socket file descriptors via LISTEN_FDS/LISTEN_PID environment
- [x] Socket activation trigger (async poll, start service on connection)
- StartTransientUnit for socket units - not implementing (only used by systemd-run for testing; no boot services need it)

### M11: Additional Unit Types
- [x] .mount units (9 units) - parse and execute mount operations
- [x] .slice units (7 units) - cgroup hierarchy organization (ordering for slices.target)
- .path units - not implementing (inotify-based file watching; only 3 units, low value)
- .automount units - not implementing (autofs lazy mounting; only 1 unit for binfmt_misc)
- .swap units - not implementing (generated by fstab-generator; use /etc/fstab directly)
- implicit slices (-.slice, system.slice) - not implementing (cgroups created directly, no unit needed)
- [x] .timer units (11 units) - scheduled activation (OnCalendar, OnBootSec, OnUnitActiveSec)

### M12: Additional Conditions ✓
| Condition | Priority | Status | Notes |
|-----------|----------|--------|-------|
| ConditionVirtualization= | high | ✓ done | 12 uses, detect container/VM (docker, podman, qemu, vmware, etc.) |
| ConditionCapability= | high | ✓ done | 14 uses, check process caps (CAP_SYS_ADMIN, etc.) |
| ConditionKernelCommandLine= | medium | ✓ done | 4 uses, check /proc/cmdline |
| ConditionSecurity= | low | ✓ done | 20 uses, SELinux/AppArmor/SMACK/TOMOYO/IMA/audit |
| ConditionFirstBoot= | low | ✓ done | 2 uses, first boot detection (/run/systemd/first-boot or machine-id) |
| ConditionNeedsUpdate= | low | ✓ done | 6 uses, /etc or /var mtime vs /var/lib/systemd/update-done.d/ flag |

### M13: User Sessions ✓
For full desktop support (systemd --user equivalent).
- [x] Per-user service manager instances (`sysd --user`)
- [x] User unit search paths (~/.config/systemd/user, /etc/systemd/user, /usr/lib/systemd/user)
- [x] XDG_RUNTIME_DIR management (ensure /run/user/<uid> exists)
- [x] User socket path (/run/user/<uid>/sysd.sock)
- [x] Lingering support check (Manager::is_lingering)
- [x] sysdctl --user support

### M14: Logind Scope Support (5 uses)
Complete StartTransientUnit for logind session scopes.
- [x] Create cgroup: /sys/fs/cgroup/{slice}/{name}/ via ScopeManager
- [x] Move PIDs into scope cgroup (CgroupManager::add_pids)
- [x] Parse scope properties: Description, Slice, PIDs (parse_scope_properties)
- [x] Register D-Bus object for scope unit (org.freedesktop.systemd1.Scope)
- [x] Scope.Abandon() method (ScopeInterface::abandon)
- [x] Track scope state in Manager for status/list queries

Logind creates scopes like `session-1.scope` under `user-1000.slice` for login sessions.

### Fstab Support (built-in, replaces fstab-generator)
- [x] Parse /etc/fstab at startup
- [x] Generate Mount units from fstab entries
- [x] Skip swap entries and noauto mounts
- [x] Handle network mounts (nfs, cifs) with network-online.target dependency
- [x] Handle bind mounts with source dependency
- [x] Load in Manager::load_fstab() at startup

### Getty Support (built-in, replaces getty-generator)
- [x] Parse /proc/cmdline for console= parameters
- [x] Create serial-getty@ttyS0.service for serial consoles
- [x] Create getty@tty1.service for virtual consoles
- [x] Support baud rate parsing (e.g., console=ttyS0,115200)
- [x] Default to tty1-tty6 if no console= parameters
- [x] Load in Manager::load_gettys() at startup

### M15: Remaining Directives ✓
Counts in parentheses are from enabled units on target system.
- [x] DevicePolicy= + DeviceAllow= (7 uses) - mount namespace device isolation with r/rw bind mounts
- [x] SystemCallFilter= enforcement (4 uses) - seccomp BPF filter applied
- [x] RestrictNamespaces= enforcement (4 uses) - seccomp blocks unshare/clone with namespace flags
- [x] NotifyAccess= (10 uses) - SO_PEERCRED validation in validate_notify_access()
- [x] UnsetEnvironment= (1 use) - cmd.env_remove() for specified variables
- [x] DefaultInstance= (1 use) - applied when loading bare template units
- [x] TimeoutStartSec=/TimeoutStopSec= (2 uses each) - separate start/stop timeouts (already implemented)

### M16: Extended Security Hardening (DONE)
Additional security directives used in enabled units.
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| RestrictRealtime= | 5 | DONE | Block realtime scheduling (seccomp) |
| ProtectControlGroups= | 5 | DONE | Read-only /sys/fs/cgroup (bind mount) |
| MemoryDenyWriteExecute= | 5 | DONE | Block W+X memory (PR_SET_MDWE) |
| SystemCallErrorNumber= | 4 | DONE | Error code for blocked syscalls |
| SystemCallArchitectures= | 4 | DONE | Restrict syscall ABIs (native logged) |
| RestrictAddressFamilies= | 4 | DONE | Restrict socket types (seccomp deny list) |
| LockPersonality= | 4 | DONE | Lock execution domain (seccomp) |
| RestrictSUIDSGID= | 3 | DONE | Block setuid/setgid file creation (seccomp) |
| ProtectKernelTunables= | 3 | DONE | Read-only /proc/sys, /sys (bind mount) |
| ProtectKernelLogs= | 3 | DONE | Block /dev/kmsg, /proc/kmsg (inaccessible) |
| ProtectClock= | 2 | DONE | Block clock_settime, adjtimex (seccomp) |
| ProtectHostname= | 1 | DONE | Block sethostname, setdomainname (seccomp) |
| IgnoreSIGPIPE= | 2 | DONE | Set SIG_IGN for SIGPIPE |

### M17: Runtime Directories & Resource Limits ✓
Auto-created directories and resource constraints.
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| StateDirectory= | 6 | DONE | Auto-create /var/lib/<name> with chown |
| RuntimeDirectory= | 4 | DONE | Auto-create /run/<name> with chown |
| LimitNPROC= | 3 | DONE | Max processes via setrlimit(RLIMIT_NPROC) |
| ConfigurationDirectory= | 2 | DONE | Auto-create /etc/<name> with chown |
| RuntimeDirectoryPreserve= | 2 | DONE | no/yes/restart cleanup modes |
| LogsDirectory= | 1 | DONE | Auto-create /var/log/<name> with chown |
| CacheDirectory= | 1 | DONE | Auto-create /var/cache/<name> with chown |
| LimitCORE= | 1 | DONE | Core dump size via setrlimit(RLIMIT_CORE) |
| Group= | 1 | DONE | setgid() before setuid() |
| DynamicUser= | 1 | DONE | M19: DynamicUserManager allocates 61184-65519 range |

### M18: Process Control & Dependencies ✓
Restart behavior, signals, and unit relationships.
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| StartLimitBurst= | 2 | DONE | Restart rate limit burst count |
| StartLimitIntervalSec= | 1 | DONE | Restart rate limit window (default 10s) |
| Sockets= | 2 | DONE | Explicit socket association for multi-socket services |
| SendSIGHUP= | 2 | DONE | Send SIGHUP before SIGTERM |
| Slice= | 1 | DONE | Explicit cgroup slice placement |
| Delegate= | 1 | DONE | M19: enable_delegation() for cgroup subtree |
| DevicePolicy= | 1 | DONE | Device access via mount namespace isolation |
| BindsTo= | 1 | DONE | M19: propagate_binds_to_stop() |
| ExecStopPost= | 1 | DONE | Run commands after service stops |
| FileDescriptorStoreMax= | 1 | DONE | M19: FD store via FDSTORE=1 + SCM_RIGHTS |
| IgnoreOnIsolate= | 1 | WONTFIX | Unit overriding admin intent; can stop manually |
| RestartPreventExitStatus= | 1 | DONE | Skip restart for specific exit codes |

### M19: Remaining Stubs & Polish ✓
Complete remaining stubs and minor features from earlier milestones.

**Easy:**
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| DefaultInstance= | 2 | DONE | Template loading uses DefaultInstance when no instance specified |

**Moderate:**
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| NotifyAccess= | 10 | DONE | SO_PEERCRED validation in validate_notify_access() |
| BindsTo= | 1 | DONE | propagate_binds_to_stop() stops dependent units |

**Complex:**
| Directive | Uses | Status | Notes |
|-----------|------|--------|-------|
| DynamicUser= | 1 | DONE | DynamicUserManager allocates from 61184-65519 range |
| Delegate= | 1 | DONE | enable_delegation() writes to cgroup.subtree_control |
| FileDescriptorStoreMax= | 1 | DONE | FD store via SCM_RIGHTS, restored on restart |

### M20: Deferred & Polish ✓
Low-priority items deferred from earlier milestones.

**Commands:**
| Command | Status | Notes |
|---------|--------|-------|
| sysdctl reload | DONE | Manager::reload_units() re-parses all unit files |
| sysdctl switch-target | DONE | Manager::switch_target() stops unrelated units |
| sysdctl sync | DONE | Manager::sync_units() reloads + restarts changed |

**Features:**
| Feature | Status | Notes |
|---------|--------|-------|
| User mode D-Bus | WONTFIX | D-Bus is for logind; logind is system-level only |
| BootPlan expansion | DONE | get_boot_plan() resolves dependencies for --dry-run |
| Restart tracking | WONTFIX | RuntimeDirectoryPreserve=restart has 0 real-world uses |

### Generators
Not needed - sysd has built-in fstab and getty generators.
- [x] systemd-fstab-generator → Built-in `fstab.rs`
- [x] systemd-getty-generator → Built-in `getty.rs`
- External generators not supported (not needed for minimal systems)

**System generators (Arch Linux):**
| Generator | Purpose | Sysd Status |
|-----------|---------|-------------|
| systemd-fstab-generator | /etc/fstab → .mount units | Built-in |
| systemd-getty-generator | console= → getty services | Built-in |
| systemd-cryptsetup-generator | /etc/crypttab → LUKS units | Not needed |
| systemd-gpt-auto-generator | GPT partition discovery | Not needed |
| systemd-hibernate-resume-generator | Resume from hibernation | Not needed |
| systemd-bless-boot-generator | Boot counting/A-B updates | Not needed |
| systemd-debug-generator | systemd.debug_shell param | Not needed |
| systemd-run-generator | systemd.run= param | Not needed |
| systemd-ssh-generator | SSH socket activation | Not needed |
| systemd-system-update-generator | Offline updates | Not needed |
| systemd-tpm2-generator | TPM2 credentials | Not needed |
| systemd-veritysetup-generator | dm-verity setup | Not needed |
| systemd-integritysetup-generator | dm-integrity setup | Not needed |
| systemd-import-generator | Import images | Not needed |
| systemd-factory-reset-generator | Factory reset | Not needed |

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
