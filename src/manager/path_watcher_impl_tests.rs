use super::*;
use std::ffi::OsString;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempRoot(std::path::PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(label: &str) -> TempRoot {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sysd-path-watcher-{label}-{}-{counter}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempRoot(dir)
}

fn watch(path: impl Into<String>, watch_type: WatchType) -> PathWatch {
    PathWatch {
        path: path.into(),
        watch_type,
    }
}

#[test]
fn initial_conditions_match_existing_paths_globs_and_non_empty_dirs() {
    let root = temp_dir("initial");
    let file = root.0.join("ready.txt");
    let non_empty_dir = root.0.join("queue");
    let empty_dir = root.0.join("empty");
    std::fs::write(&file, "ready").unwrap();
    std::fs::create_dir_all(&non_empty_dir).unwrap();
    std::fs::create_dir_all(&empty_dir).unwrap();
    std::fs::write(non_empty_dir.join("job"), "1").unwrap();

    assert!(initial_condition_satisfied(&watch(
        file.to_string_lossy(),
        WatchType::Exists
    )));
    assert!(initial_condition_satisfied(&watch(
        root.0.join("*.txt").to_string_lossy(),
        WatchType::ExistsGlob
    )));
    assert!(initial_condition_satisfied(&watch(
        non_empty_dir.to_string_lossy(),
        WatchType::DirectoryNotEmpty
    )));
    assert!(!initial_condition_satisfied(&watch(
        empty_dir.to_string_lossy(),
        WatchType::DirectoryNotEmpty
    )));
    assert!(!initial_condition_satisfied(&watch(
        root.0.join("missing").to_string_lossy(),
        WatchType::Exists
    )));
    assert!(!initial_condition_satisfied(&watch(
        root.0.join("missing-*").to_string_lossy(),
        WatchType::ExistsGlob
    )));
    assert!(!initial_condition_satisfied(&watch(
        file.to_string_lossy(),
        WatchType::Changed
    )));
    assert!(!initial_condition_satisfied(&watch(
        file.to_string_lossy(),
        WatchType::Modified
    )));
}

#[tokio::test]
async fn trigger_initial_conditions_sends_first_matching_watch() {
    let root = temp_dir("trigger-initial");
    let first = root.0.join("first");
    let second = root.0.join("second");
    std::fs::write(&first, "first").unwrap();
    std::fs::write(&second, "second").unwrap();
    let watches = vec![
        watch(root.0.join("missing").to_string_lossy(), WatchType::Exists),
        watch(first.to_string_lossy(), WatchType::Exists),
        watch(second.to_string_lossy(), WatchType::Exists),
    ];
    let (tx, mut rx) = mpsc::channel(1);

    assert!(trigger_initial_conditions("ready.path", "ready.service", &watches, &tx).await);

    let message = rx.recv().await.unwrap();
    assert_eq!(message.path_name, "ready.path");
    assert_eq!(message.service_name, "ready.service");
    assert_eq!(message.triggered_path, first.to_string_lossy());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn trigger_initial_conditions_reports_no_match_without_sending() {
    let root = temp_dir("trigger-none");
    let watches = vec![watch(
        root.0.join("missing").to_string_lossy(),
        WatchType::Exists,
    )];
    let (tx, mut rx) = mpsc::channel(1);

    assert!(!trigger_initial_conditions("missing.path", "missing.service", &watches, &tx).await);
    assert!(rx.try_recv().is_err());
}

#[test]
fn watch_target_and_mask_selects_paths_for_each_watch_type() {
    let root = temp_dir("targets");
    let dir = root.0.join("dir");
    let file = root.0.join("file");
    let missing = root.0.join("missing");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&file, "data").unwrap();

    let (target, mask) = watch_target_and_mask(&watch(file.to_string_lossy(), WatchType::Exists));
    assert_eq!(target, root.0);
    assert!(mask.contains(inotify::WatchMask::CREATE));
    assert!(mask.contains(inotify::WatchMask::MOVED_TO));

    let (target, mask) = watch_target_and_mask(&watch(
        root.0.join("*.conf").to_string_lossy(),
        WatchType::ExistsGlob,
    ));
    assert_eq!(target, root.0);
    assert!(mask.contains(inotify::WatchMask::CREATE));

    let (target, mask) = changed_watch_target(&dir);
    assert_eq!(target, dir);
    assert!(mask.contains(inotify::WatchMask::DELETE));
    assert!(mask.contains(inotify::WatchMask::MOVED_FROM));

    let (target, mask) = changed_watch_target(&missing);
    assert_eq!(target, root.0);
    assert!(mask.contains(inotify::WatchMask::CREATE));
    assert!(!mask.contains(inotify::WatchMask::DELETE));

    let (target, mask) = modified_watch_target(&missing);
    assert_eq!(target, root.0);
    assert!(mask.contains(inotify::WatchMask::MOVED_TO));

    let (target, mask) = directory_not_empty_watch_target(&root.0.join("not-created/item"));
    assert_eq!(target, root.0.join("not-created"));
    assert_eq!(mask, inotify::WatchMask::CREATE);
}

#[test]
fn watch_condition_satisfied_matches_current_filesystem_state() {
    let root = temp_dir("watch-condition");
    let file = root.0.join("exists");
    let queue = root.0.join("queue");
    let empty = root.0.join("empty");
    std::fs::write(&file, "data").unwrap();
    std::fs::create_dir_all(&queue).unwrap();
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(queue.join("job"), "1").unwrap();

    assert!(watch_condition_satisfied(&WatchedPath {
        path: file.to_string_lossy().into_owned(),
        watch_type: WatchType::Exists,
    }));
    assert!(watch_condition_satisfied(&WatchedPath {
        path: root.0.join("*").to_string_lossy().into_owned(),
        watch_type: WatchType::ExistsGlob,
    }));
    assert!(watch_condition_satisfied(&WatchedPath {
        path: file.to_string_lossy().into_owned(),
        watch_type: WatchType::Changed,
    }));
    assert!(watch_condition_satisfied(&WatchedPath {
        path: file.to_string_lossy().into_owned(),
        watch_type: WatchType::Modified,
    }));
    assert!(watch_condition_satisfied(&WatchedPath {
        path: queue.to_string_lossy().into_owned(),
        watch_type: WatchType::DirectoryNotEmpty,
    }));
    assert!(!watch_condition_satisfied(&WatchedPath {
        path: empty.to_string_lossy().into_owned(),
        watch_type: WatchType::DirectoryNotEmpty,
    }));
}

#[test]
fn register_watches_skips_missing_targets_and_registers_existing_targets() {
    let root = temp_dir("register");
    let file = root.0.join("file");
    std::fs::write(&file, "data").unwrap();
    let inotify = inotify::Inotify::init().unwrap();
    let watches = vec![
        watch(file.to_string_lossy(), WatchType::Modified),
        watch(root.0.join("missing/item").to_string_lossy(), WatchType::Changed),
    ];

    let registered = register_watches("register.path", &inotify, &watches);

    assert_eq!(registered.len(), 1);
    let watched = registered.values().next().unwrap();
    assert_eq!(watched.path, file.to_string_lossy());
    assert_eq!(watched.watch_type, WatchType::Modified);
}

#[tokio::test]
async fn handle_event_sends_trigger_when_watch_condition_is_satisfied() {
    let root = temp_dir("handle-event");
    let file = root.0.join("file");
    std::fs::write(&file, "data").unwrap();
    let inotify = inotify::Inotify::init().unwrap();
    let registered = register_watches(
        "changed.path",
        &inotify,
        &[watch(file.to_string_lossy(), WatchType::Changed)],
    );
    let wd = registered.keys().next().unwrap().clone();
    let event = inotify::Event {
        wd,
        mask: inotify::EventMask::MODIFY,
        cookie: 0,
        name: Some(OsString::from("file")),
    };
    let (tx, mut rx) = mpsc::channel(1);

    handle_event("changed.path", "changed.service", &tx, &registered, event).await;

    let message = rx.recv().await.unwrap();
    assert_eq!(message.path_name, "changed.path");
    assert_eq!(message.service_name, "changed.service");
    assert_eq!(message.triggered_path, file.to_string_lossy());
}

#[tokio::test]
async fn watch_paths_returns_after_initial_condition_or_empty_watch_map() {
    let root = temp_dir("watch-paths");
    let file = root.0.join("ready");
    std::fs::write(&file, "ready").unwrap();
    let (tx, mut rx) = mpsc::channel(1);

    watch_paths(
        "ready.path".to_string(),
        "ready.service".to_string(),
        vec![watch(file.to_string_lossy(), WatchType::Exists)],
        tx,
    )
    .await;

    assert_eq!(rx.recv().await.unwrap().triggered_path, file.to_string_lossy());

    let (tx, mut rx) = mpsc::channel(1);
    watch_paths(
        "empty.path".to_string(),
        "empty.service".to_string(),
        vec![watch(
            root.0.join("missing/item").to_string_lossy(),
            WatchType::Changed,
        )],
        tx,
    )
    .await;

    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn watch_paths_triggers_from_inotify_events() {
    let root = temp_dir("watch-events");
    let file = root.0.join("watched");
    std::fs::write(&file, "before").unwrap();
    let (tx, mut rx) = mpsc::channel(1);

    let handle = tokio::spawn(watch_paths(
        "modified.path".to_string(),
        "modified.service".to_string(),
        vec![watch(file.to_string_lossy(), WatchType::Modified)],
        tx,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    std::fs::write(&file, "after").unwrap();

    let message = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.path_name, "modified.path");
    assert_eq!(message.service_name, "modified.service");
    assert_eq!(message.triggered_path, file.to_string_lossy());

    handle.abort();
}

#[tokio::test]
async fn watch_paths_triggers_when_glob_match_is_created() {
    let root = temp_dir("watch-glob");
    let pattern = root.0.join("*.ready");
    let (tx, mut rx) = mpsc::channel(1);

    let handle = tokio::spawn(watch_paths(
        "glob.path".to_string(),
        "glob.service".to_string(),
        vec![watch(pattern.to_string_lossy(), WatchType::ExistsGlob)],
        tx,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    std::fs::write(root.0.join("job.ready"), "ready").unwrap();

    let message = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.path_name, "glob.path");
    assert_eq!(message.service_name, "glob.service");
    assert_eq!(message.triggered_path, pattern.to_string_lossy());

    handle.abort();
}
