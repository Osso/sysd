//! Path watcher for .path units
//!
//! Watches filesystem paths using inotify and triggers service activation
//! when specified conditions are met.

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

/// Watch filesystem paths and send activation messages when conditions are met
pub async fn watch_paths(
    path_name: String,
    service_name: String,
    watches: Vec<PathWatch>,
    tx: mpsc::Sender<PathTriggered>,
) {
    // First, check if any PathExists conditions are already satisfied
    for watch in &watches {
        if watch.watch_type == WatchType::Exists {
            if Path::new(&watch.path).exists() {
                log::info!(
                    "{}: path {} exists, triggering {}",
                    path_name,
                    watch.path,
                    service_name
                );
                let _ = tx
                    .send(PathTriggered {
                        path_name: path_name.clone(),
                        service_name: service_name.clone(),
                        triggered_path: watch.path.clone(),
                    })
                    .await;
                return;
            }
        } else if watch.watch_type == WatchType::DirectoryNotEmpty {
            if let Ok(mut entries) = std::fs::read_dir(&watch.path) {
                if entries.next().is_some() {
                    log::info!(
                        "{}: directory {} not empty, triggering {}",
                        path_name,
                        watch.path,
                        service_name
                    );
                    let _ = tx
                        .send(PathTriggered {
                            path_name: path_name.clone(),
                            service_name: service_name.clone(),
                            triggered_path: watch.path.clone(),
                        })
                        .await;
                    return;
                }
            }
        } else if watch.watch_type == WatchType::ExistsGlob {
            if let Ok(paths) = glob::glob(&watch.path) {
                if paths.into_iter().any(|p| p.is_ok()) {
                    log::info!(
                        "{}: glob {} matched, triggering {}",
                        path_name,
                        watch.path,
                        service_name
                    );
                    let _ = tx
                        .send(PathTriggered {
                            path_name: path_name.clone(),
                            service_name: service_name.clone(),
                            triggered_path: watch.path.clone(),
                        })
                        .await;
                    return;
                }
            }
        }
    }

    // Set up inotify watches for paths that need monitoring
    let inotify = match inotify::Inotify::init() {
        Ok(i) => i,
        Err(e) => {
            log::error!("{}: failed to initialize inotify: {}", path_name, e);
            return;
        }
    };

    let mut watch_map: HashMap<inotify::WatchDescriptor, (String, WatchType)> = HashMap::new();

    for watch in &watches {
        let path = Path::new(&watch.path);

        // Determine what to watch - the path itself or its parent
        let (watch_path, watch_mask) = match watch.watch_type {
            WatchType::Exists => {
                // Watch parent directory for file creation
                let parent = path.parent().unwrap_or(Path::new("/"));
                (
                    parent.to_path_buf(),
                    inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                )
            }
            WatchType::ExistsGlob => {
                // For globs, watch the base directory
                let base = glob_base_dir(&watch.path);
                (
                    base,
                    inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                )
            }
            WatchType::Changed => {
                // Watch for any changes
                if path.is_dir() {
                    (
                        path.to_path_buf(),
                        inotify::WatchMask::CREATE
                            | inotify::WatchMask::DELETE
                            | inotify::WatchMask::MODIFY
                            | inotify::WatchMask::MOVED_TO
                            | inotify::WatchMask::MOVED_FROM
                            | inotify::WatchMask::ATTRIB,
                    )
                } else if path.exists() {
                    (
                        path.to_path_buf(),
                        inotify::WatchMask::MODIFY
                            | inotify::WatchMask::ATTRIB
                            | inotify::WatchMask::CLOSE_WRITE,
                    )
                } else {
                    // Watch parent for creation
                    let parent = path.parent().unwrap_or(Path::new("/"));
                    (
                        parent.to_path_buf(),
                        inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                    )
                }
            }
            WatchType::Modified => {
                // Watch for content modifications only
                if path.exists() {
                    (
                        path.to_path_buf(),
                        inotify::WatchMask::MODIFY | inotify::WatchMask::CLOSE_WRITE,
                    )
                } else {
                    let parent = path.parent().unwrap_or(Path::new("/"));
                    (
                        parent.to_path_buf(),
                        inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                    )
                }
            }
            WatchType::DirectoryNotEmpty => {
                // Watch directory for new files
                if path.is_dir() {
                    (
                        path.to_path_buf(),
                        inotify::WatchMask::CREATE | inotify::WatchMask::MOVED_TO,
                    )
                } else {
                    // Watch parent for directory creation
                    let parent = path.parent().unwrap_or(Path::new("/"));
                    (parent.to_path_buf(), inotify::WatchMask::CREATE)
                }
            }
        };

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
                watch_map.insert(wd, (watch.path.clone(), watch.watch_type));
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

    if watch_map.is_empty() {
        log::warn!("{}: no watches could be established", path_name);
        return;
    }

    // Convert to async stream
    let mut buffer = [0; 4096];
    let mut stream = inotify
        .into_event_stream(&mut buffer)
        .expect("Failed to create inotify event stream");

    use futures_lite::StreamExt;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                if let Some((watched_path, watch_type)) = watch_map.get(&event.wd) {
                    log::debug!(
                        "{}: inotify event {:?} on {} (watching {})",
                        path_name,
                        event.mask,
                        event
                            .name
                            .as_ref()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default(),
                        watched_path
                    );

                    // Check if the condition is now satisfied
                    let triggered = match watch_type {
                        WatchType::Exists => Path::new(watched_path).exists(),
                        WatchType::ExistsGlob => glob::glob(watched_path)
                            .map(|paths| paths.into_iter().any(|p| p.is_ok()))
                            .unwrap_or(false),
                        WatchType::Changed | WatchType::Modified => true,
                        WatchType::DirectoryNotEmpty => std::fs::read_dir(watched_path)
                            .map(|mut entries| entries.next().is_some())
                            .unwrap_or(false),
                    };

                    if triggered {
                        log::info!(
                            "{}: path condition {:?} triggered for {}, activating {}",
                            path_name,
                            watch_type,
                            watched_path,
                            service_name
                        );
                        let _ = tx
                            .send(PathTriggered {
                                path_name: path_name.clone(),
                                service_name: service_name.clone(),
                                triggered_path: watched_path.clone(),
                            })
                            .await;
                        // Continue watching for repeated activations
                    }
                }
            }
            Err(e) => {
                log::error!("{}: inotify error: {}", path_name, e);
                break;
            }
        }
    }
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
