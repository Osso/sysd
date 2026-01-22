# sysd - Minimal systemd-compatible init

A Rust init system that parses systemd unit files and provides D-Bus compatibility for logind.

## Project Structure

```
src/
├── bin/
│   ├── sysd.rs          # Main daemon (runs as PID 1)
│   └── sysdctl.rs       # CLI client
├── manager/             # Service lifecycle management
│   ├── mod.rs           # Manager struct, start/stop/restart
│   ├── deps.rs          # Dependency resolution, topological sort
│   ├── process.rs       # Process spawning with sandbox
│   ├── sandbox.rs       # Seccomp, namespaces, capabilities
│   ├── socket_ops.rs    # Socket unit operations
│   ├── timer_ops.rs     # Timer unit operations
│   └── scope.rs         # Transient scopes for logind
├── units/               # Unit file parsing
│   ├── mod.rs           # Parser entry point
│   ├── service.rs       # ServiceType, RestartPolicy enums
│   ├── socket.rs        # Socket unit structs
│   └── timer.rs         # Timer/calendar specs
├── dbus/                # D-Bus interface (org.freedesktop.systemd1)
├── pid1/                # PID 1 responsibilities
│   ├── mount.rs         # Essential filesystem mounting
│   ├── reaper.rs        # Zombie process reaping
│   └── signals.rs       # SIGTERM/SIGINT handling
├── cgroups/             # cgroups v2 management
└── fstab.rs             # /etc/fstab → mount unit generator
```

## Build & Test

```bash
# Build
cargo build --release

# Run all tests
./run-tests.sh

# Or run specific test suites:
./run-tests.sh --unit     # Unit tests only (156 tests, fast)
./run-tests.sh --docker   # Docker integration tests (Arch Linux units)
./run-tests.sh --qemu     # QEMU integration tests (boots as PID 1)
./run-tests.sh --btrfs    # QEMU btrfs mount tests
```

**Note:** Integration tests in `tests/*.rs` require write access to `/tmp` and may fail in sandboxed environments. Use `./run-tests.sh` for full integration testing.

## Key Concepts

### Unit Types
- `.service` - Long-running daemons
- `.socket` - Socket activation (creates socket, starts service on connection)
- `.timer` - Scheduled activation (like cron)
- `.target` - Synchronization points (no process)
- `.mount` - Filesystem mounts (generated from fstab)
- `.slice` - cgroup resource limits

### Service Types (`Type=`)
- `simple` - Ready immediately after exec
- `forking` - Ready when main process exits, reads PIDFile
- `notify` / `notify-reload` - Ready on sd_notify READY=1
- `dbus` - Ready when BusName appears on D-Bus
- `oneshot` - Run once, no main process
- `idle` - Like simple, waits for job queue empty

### Boot Sequence
1. sysd starts as PID 1, mounts essential filesystems
2. Spawns zombie reaper and signal handler tasks
3. Boots to default.target (usually graphical.target)
4. D-Bus connection retries with backoff until dbus-broker ready
5. Registers org.freedesktop.systemd1 for logind compatibility

## Common Patterns

### Adding a new unit directive
1. Add field to struct in `src/units/service.rs` (or socket/timer.rs)
2. Parse it in `src/units/mod.rs` under the appropriate section
3. Use it in `src/manager/` (process.rs for exec, sandbox.rs for security)

### Adding a new ServiceType
1. Add variant to `ServiceType` enum in `src/units/service.rs`
2. Add parse case in `ServiceType::parse()`
3. Handle in `Manager::start_single()` in `src/manager/mod.rs`

## Testing Rules

**Always run `./run-tests.sh --unit` after making code changes.** Unit tests are fast (~0.01s) and catch most regressions.

- Unit tests are in each module (run with `cargo test`)
- Integration tests in `tests/` directory
- QEMU tests boot real kernel with sysd as init
- Docker tests verify unit parsing against real Arch systemd files

## IPC Protocol

sysd listens on `/run/sysd.sock` (or `/run/user/<uid>/sysd.sock` for user mode).
Uses MessagePack over Unix socket with peer credentials.
See `src/protocol.rs` for Request/Response enums.

## Implementation Notes

### Drop-in Override Files
- Location: `/etc/systemd/system/<unit>.d/*.conf`
- systemd reset convention: `Key=` (empty value) clears all previous values for that key
- Implemented in `merge_parsed_files()` in `src/units/mod.rs`

### Oneshot Services
- Oneshot services run asynchronously to avoid blocking socket activation
- Completion is handled via `OneshotCompletion` messages sent through a channel
- The background task in `sysd.rs` processes completion messages
- Multi-command oneshots run commands sequentially via the completion handler

### Notify Socket (`/run/sysd/notify`)
- sd_notify() messages from services are received via Unix datagram socket
- **Critical**: When converting nix::Errno to std::io::Error, use `from_raw_os_error(e as i32)`
  to preserve the raw OS error code. Using `Error::new(ErrorKind::Other, e)` loses the code.
- Must check `e.raw_os_error() == Some(libc::EAGAIN)` for EAGAIN handling
- SCM_CREDENTIALS provides sender PID for verification
- Journald, sshd, dbus-broker, logind all send READY=1 via this mechanism

### Boot Lock Contention
- The boot loop releases the manager lock between starting each unit
- This allows socket activation and other background tasks to run during boot
- Oneshot services don't block - they spawn a task to wait for completion

### sysd-executor
- Helper binary that applies ExecConfig (sandbox, environment, etc.) before exec
- Must be included in initramfs for QEMU tests
- Receives configuration via memfd serialized with rmp-serde

### pam_systemd.so and logind Integration
- pam_systemd.so calls logind via D-Bus when users log in (SSH, getty, etc.)
- logind calls sysd's StartTransientUnit to create session scopes (e.g., `session-1.scope`)
- Session scopes are created in user slices (e.g., `/sys/fs/cgroup/user-0.slice/session-1.scope`)
- Requires: working D-Bus (dbus-broker), logind receiving READY=1, sysd D-Bus interface
