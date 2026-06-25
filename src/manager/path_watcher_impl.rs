// Path watcher for .path units
//
// Watches filesystem paths using inotify and triggers service activation
// when specified conditions are met.

use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

/// Message sent when a path condition is triggered
#[derive(Debug)]
pub struct PathTriggered {
    /// Name of the path unit
    pub path_name: String,
    /// Name of the service to start
    pub service_name: String,
    /// The path that triggered activation
    pub triggered_path: String,
}

/// Watch type for path conditions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchType {
    /// PathExists - trigger when path exists
    Exists,
    /// PathExistsGlob - trigger when glob matches
    ExistsGlob,
    /// PathChanged - trigger on any change (inotify)
    Changed,
    /// PathModified - trigger on content modification
    Modified,
    /// DirectoryNotEmpty - trigger when directory has contents
    DirectoryNotEmpty,
}

/// Configuration for a path watch
#[derive(Debug, Clone)]
pub struct PathWatch {
    pub path: String,
    pub watch_type: WatchType,
}

#[derive(Debug, Clone)]
struct WatchedPath {
    path: String,
    watch_type: WatchType,
}

/// Watch filesystem paths and send activation messages when conditions are met
pub async fn watch_paths(
    path_name: String,
    service_name: String,
    watches: Vec<PathWatch>,
    tx: mpsc::Sender<PathTriggered>,
) {
    if trigger_initial_conditions(&path_name, &service_name, &watches, &tx).await {
        return;
    }

    let inotify = match inotify::Inotify::init() {
        Ok(i) => i,
        Err(e) => {
            log::error!("{}: failed to initialize inotify: {}", path_name, e);
            return;
        }
    };

    let watch_map = register_watches(&path_name, &inotify, &watches);
    if watch_map.is_empty() {
        log::warn!("{}: no watches could be established", path_name);
        return;
    }

    run_event_loop(path_name, service_name, tx, inotify, watch_map).await;
}

async fn trigger_initial_conditions(
    path_name: &str,
    service_name: &str,
    watches: &[PathWatch],
    tx: &mpsc::Sender<PathTriggered>,
) -> bool {
    for watch in watches {
        if !initial_condition_satisfied(watch) {
            continue;
        }

        log::info!(
            "{}: initial {:?} condition matched for {}, triggering {}",
            path_name,
            watch.watch_type,
            watch.path,
            service_name
        );
        send_trigger(tx, path_name, service_name, &watch.path).await;
        return true;
    }
    false
}

fn initial_condition_satisfied(watch: &PathWatch) -> bool {
    match watch.watch_type {
        WatchType::Exists => Path::new(&watch.path).exists(),
        WatchType::ExistsGlob => glob::glob(&watch.path)
            .map(|paths| paths.into_iter().any(|p| p.is_ok()))
            .unwrap_or(false),
        WatchType::DirectoryNotEmpty => std::fs::read_dir(&watch.path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
        WatchType::Changed | WatchType::Modified => false,
    }
}

fn register_watches(
    path_name: &str,
    inotify: &inotify::Inotify,
    watches: &[PathWatch],
) -> HashMap<inotify::WatchDescriptor, WatchedPath> {
    let mut watch_map = HashMap::new();

    for watch in watches {
        let (watch_path, watch_mask) = watch_target_and_mask(watch);
        if !watch_path.exists() {
            log::debug!(
                "{}: watch path {} does not exist, skipping",
                path_name,
                watch_path.display()
            );
            continue;
        }

        match inotify.watches().add(&watch_path, watch_mask) {
            Ok(wd) => {
                log::debug!(
                    "{}: watching {} for {:?}",
                    path_name,
                    watch_path.display(),
                    watch.watch_type
                );
                watch_map.insert(
                    wd,
                    WatchedPath {
                        path: watch.path.clone(),
                        watch_type: watch.watch_type,
                    },
                );
            }
            Err(e) => {
                log::warn!(
                    "{}: failed to watch {}: {}",
                    path_name,
                    watch_path.display(),
                    e
                );
            }
        }
    }

    watch_map
}

fn watch_target_and_mask(watch: &PathWatch) -> (std::path::PathBuf, inotify::WatchMask) {
    let path = Path::new(&watch.path);

    match watch.watch_type {
        WatchType::Exists => (
            path.parent().unwrap_or(Path::new("/")).to_path_buf(),
            inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
        ),
        WatchType::ExistsGlob => (
            glob_base_dir(&watch.path),
            inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
        ),
        WatchType::Changed => changed_watch_target(path),
        WatchType::Modified => modified_watch_target(path),
        WatchType::DirectoryNotEmpty => directory_not_empty_watch_target(path),
    }
}

fn changed_watch_target(path: &Path) -> (std::path::PathBuf, inotify::WatchMask) {
    if path.is_dir() {
        return (
            path.to_path_buf(),
            inotify::WatchMask::CREATE
                | inotify::WatchMask::DELETE
                | inotify::WatchMask::MODIFY
                | inotify::WatchMask::MOVED_TO
                | inotify::WatchMask::MOVED_FROM
                | inotify::WatchMask::ATTRIB,
        );
    }

    if path.exists() {
        return (
            path.to_path_buf(),
            inotify::WatchMask::MODIFY | inotify::WatchMask::ATTRIB | inotify::WatchMask::CLOSE_WRITE,
        );
    }

    (
        path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
    )
}

fn modified_watch_target(path: &Path) -> (std::path::PathBuf, inotify::WatchMask) {
    if path.exists() {
        return (
            path.to_path_buf(),
            inotify::WatchMask::MODIFY | inotify::WatchMask::CLOSE_WRITE,
        );
    }

    (
        path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
    )
}

fn directory_not_empty_watch_target(path: &Path) -> (std::path::PathBuf, inotify::WatchMask) {
    if path.is_dir() {
        return (
            path.to_path_buf(),
            inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
        );
    }

    (
        path.parent().unwrap_or(Path::new("/")).to_path_buf(),
        inotify::WatchMask::CREATE,
    )
}

async fn run_event_loop(
    path_name: String,
    service_name: String,
    tx: mpsc::Sender<PathTriggered>,
    inotify: inotify::Inotify,
    watch_map: HashMap<inotify::WatchDescriptor, WatchedPath>,
) {
    let mut buffer = [0; 4096];
    let mut stream = match inotify.into_event_stream(&mut buffer) {
        Ok(stream) => stream,
        Err(e) => {
            log::error!("{}: failed to create inotify event stream: {}", path_name, e);
            return;
        }
    };

    use futures_lite::StreamExt;

    while let Some(event_result) = stream.next().await {
        let event = match event_result {
            Ok(event) => event,
            Err(e) => {
                log::error!("{}: inotify error: {}", path_name, e);
                break;
            }
        };

        handle_event(&path_name, &service_name, &tx, &watch_map, event).await;
    }
}

async fn handle_event(
    path_name: &str,
    service_name: &str,
    tx: &mpsc::Sender<PathTriggered>,
    watch_map: &HashMap<inotify::WatchDescriptor, WatchedPath>,
    event: inotify::Event<std::ffi::OsString>,
) {
    let Some(watched) = watch_map.get(&event.wd) else {
        return;
    };

    let name = event
        .name
        .as_ref()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    log::debug!(
        "{}: inotify event {:?} on {} (watching {})",
        path_name,
        event.mask,
        name,
        watched.path
    );

    if !watch_condition_satisfied(watched) {
        return;
    }

    log::info!(
        "{}: path condition {:?} triggered for {}, activating {}",
        path_name,
        watched.watch_type,
        watched.path,
        service_name
    );
    send_trigger(tx, path_name, service_name, &watched.path).await;
}

fn watch_condition_satisfied(watched: &WatchedPath) -> bool {
    match watched.watch_type {
        WatchType::Exists => Path::new(&watched.path).exists(),
        WatchType::ExistsGlob => glob::glob(&watched.path)
            .map(|paths| paths.into_iter().any(|p| p.is_ok()))
            .unwrap_or(false),
        WatchType::Changed | WatchType::Modified => true,
        WatchType::DirectoryNotEmpty => std::fs::read_dir(&watched.path)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
    }
}

async fn send_trigger(
    tx: &mpsc::Sender<PathTriggered>,
    path_name: &str,
    service_name: &str,
    triggered_path: &str,
) {
    let _ = tx
        .send(PathTriggered {
            path_name: path_name.to_string(),
            service_name: service_name.to_string(),
            triggered_path: triggered_path.to_string(),
        })
        .await;
}

/// Extract base directory from a glob pattern
fn glob_base_dir(pattern: &str) -> std::path::PathBuf {
    let path = Path::new(pattern);
    for ancestor in path.ancestors() {
        let s = ancestor.to_string_lossy();
        if !s.contains('*') && !s.contains('?') && !s.contains('[') {
            return ancestor.to_path_buf();
        }
    }
    std::path::PathBuf::from("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_base_dir() {
        assert_eq!(
            glob_base_dir("/var/cache/cups/*.conf"),
            Path::new("/var/cache/cups")
        );
        assert_eq!(
            glob_base_dir("/etc/systemd/system/*.service"),
            Path::new("/etc/systemd/system")
        );
        assert_eq!(glob_base_dir("/tmp/test"), Path::new("/tmp/test"));
        assert_eq!(glob_base_dir("/*"), Path::new("/"));
    }
}
