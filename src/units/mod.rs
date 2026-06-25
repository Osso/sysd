//! Unit file parsing and type definitions
//!
//! Parses systemd .service, .target, and .mount files into typed Rust structures.

mod mount;
mod parse_units;
mod parser;
mod path;
mod service;
mod slice;
mod socket;
mod target;
mod timer;
mod unit;

pub use mount::{Mount, MountSection};
pub use parse_units::*;
pub use parser::{parse_file, parse_unit_file, ParseError, ParsedFile};
pub use path::{Path as PathUnit, PathSection};
pub use service::*;
pub use slice::Slice;
pub use socket::{ListenType, Listener, Socket, SocketSection};
pub use target::Target;
pub use timer::{CalendarSpec, Timer, TimerSection};
pub use unit::Unit;
