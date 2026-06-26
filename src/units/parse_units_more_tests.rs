use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_unit_dir(test_name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sysd-parse-units-more-{}-{}-{}",
        test_name,
        std::process::id(),
        nonce
    ));
    fs::create_dir(&path).expect("temp unit directory should be created");
    path
}

#[tokio::test]
async fn typed_load_wrappers_parse_non_service_unit_files() {
    let dir = temp_unit_dir("typed-loaders");
    let path_unit = dir.join("demo.path");
    let slice_unit = dir.join("demo.slice");
    let mount_unit = dir.join("tmp-demo.mount");
    let socket_unit = dir.join("demo.socket");
    let timer_unit = dir.join("demo.timer");

    fs::write(
        &path_unit,
        "[Path]\nPathExists=/tmp/demo\n[Install]\nWantedBy=multi-user.target\n",
    )
    .unwrap();
    fs::write(&slice_unit, "[Slice]\nCPUQuota=25%\n").unwrap();
    fs::write(
        &mount_unit,
        "[Mount]\nWhat=tmpfs\nWhere=/tmp/demo\nType=tmpfs\nOptions=size=1m\n",
    )
    .unwrap();
    fs::write(
        &socket_unit,
        "[Socket]\nListenStream=/run/demo.sock\nAccept=yes\n",
    )
    .unwrap();
    fs::write(&timer_unit, "[Timer]\nOnBootSec=5s\nUnit=demo.service\n").unwrap();

    assert_eq!(load_path(&path_unit).await.unwrap().name, "demo.path");
    assert_eq!(load_slice(&slice_unit).await.unwrap().name, "demo.slice");
    assert_eq!(
        load_mount(&mount_unit).await.unwrap().name,
        "tmp-demo.mount"
    );
    assert_eq!(load_socket(&socket_unit).await.unwrap().name, "demo.socket");
    assert_eq!(load_timer(&timer_unit).await.unwrap().name, "demo.timer");

    fs::remove_dir_all(&dir).expect("temp unit directory should be removed");
}

#[tokio::test]
async fn load_unit_routes_all_supported_extensions_and_rejects_unknown_units() {
    let dir = temp_unit_dir("load-unit-routing");
    let target_unit = dir.join("demo.target");
    let mount_unit = dir.join("tmp-demo.mount");
    let slice_unit = dir.join("demo.slice");
    let socket_unit = dir.join("demo.socket");
    let timer_unit = dir.join("demo.timer");
    let path_unit = dir.join("demo.path");
    let unknown_unit = dir.join("demo.device");

    fs::write(&target_unit, "[Unit]\nDescription=Demo\n").unwrap();
    fs::write(
        &mount_unit,
        "[Mount]\nWhat=tmpfs\nWhere=/tmp/demo\nType=tmpfs\n",
    )
    .unwrap();
    fs::write(&slice_unit, "[Slice]\nTasksMax=8\n").unwrap();
    fs::write(&socket_unit, "[Socket]\nListenDatagram=/run/demo.sock\n").unwrap();
    fs::write(&timer_unit, "[Timer]\nOnCalendar=daily\n").unwrap();
    fs::write(&path_unit, "[Path]\nPathChanged=/tmp/demo\n").unwrap();
    fs::write(&unknown_unit, "").unwrap();

    assert!(matches!(
        load_unit(&target_unit).await.unwrap(),
        Unit::Target(_)
    ));
    assert!(matches!(
        load_unit(&mount_unit).await.unwrap(),
        Unit::Mount(_)
    ));
    assert!(matches!(
        load_unit(&slice_unit).await.unwrap(),
        Unit::Slice(_)
    ));
    assert!(matches!(
        load_unit(&socket_unit).await.unwrap(),
        Unit::Socket(_)
    ));
    assert!(matches!(
        load_unit(&timer_unit).await.unwrap(),
        Unit::Timer(_)
    ));
    assert!(matches!(
        load_unit(&path_unit).await.unwrap(),
        Unit::Path(_)
    ));
    assert!(matches!(
        load_unit(&unknown_unit).await,
        Err(ParseError::Io(_))
    ));

    fs::remove_dir_all(&dir).expect("temp unit directory should be removed");
}

#[tokio::test]
async fn load_service_uses_symlink_target_name_when_available() {
    let dir = temp_unit_dir("symlink-name");
    let target = dir.join("actual.service");
    let link = dir.join("alias.service");

    fs::write(&target, "[Service]\nExecStart=/bin/true\n").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let service = load_service(&link).await.unwrap();

    assert_eq!(service.name, "actual.service");
    fs::remove_dir_all(&dir).expect("temp unit directory should be removed");
}
