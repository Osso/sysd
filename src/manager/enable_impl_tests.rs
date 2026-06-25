use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(label: &str) -> TempRoot {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sysd-enable-{label}-{}-{counter}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempRoot(dir)
}

fn write_unit(root: &TempRoot, name: &str, contents: &str) -> PathBuf {
    let path = root.0.join(name);
    std::fs::write(&path, contents.trim_start()).unwrap();
    path
}

fn manager_with_unit_dir(root: &TempRoot) -> Manager {
    let mut manager = Manager::new_user();
    manager.unit_paths = vec![root.0.clone()];
    manager
}

fn link_target(path: &PathBuf) -> PathBuf {
    std::fs::read_link(path).unwrap()
}

#[tokio::test]
async fn enable_creates_wants_requires_alias_and_also_links() {
    let root = temp_dir("create-all");
    let main_path = write_unit(
        &root,
        "demo.service",
        r#"
[Unit]
Description=Demo

[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
RequiredBy=boot.target
Alias=demo-alias.service
Also=helper.service
"#,
    );
    let helper_path = write_unit(
        &root,
        "helper.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
Alias=helper-alias.service
"#,
    );
    let mut manager = manager_with_unit_dir(&root);

    let created = manager.enable("demo").await.unwrap();

    let created: HashSet<PathBuf> = created.into_iter().collect();
    let main_wants = root.0.join("multi-user.target.wants/demo.service");
    let main_requires = root.0.join("boot.target.requires/demo.service");
    let main_alias = root.0.join("demo-alias.service");
    let helper_wants = root.0.join("multi-user.target.wants/helper.service");
    let helper_alias = root.0.join("helper-alias.service");

    assert_eq!(created.len(), 5);
    assert!(created.contains(&main_wants));
    assert!(created.contains(&main_requires));
    assert!(created.contains(&main_alias));
    assert!(created.contains(&helper_wants));
    assert!(created.contains(&helper_alias));
    assert_eq!(link_target(&main_wants), main_path);
    assert_eq!(link_target(&main_requires), main_path);
    assert_eq!(link_target(&main_alias), main_path);
    assert_eq!(link_target(&helper_wants), helper_path);
    assert_eq!(link_target(&helper_alias), helper_path);
}

#[tokio::test]
async fn enable_replaces_existing_enable_links() {
    let root = temp_dir("replace");
    let unit_path = write_unit(
        &root,
        "replace.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
Alias=old-replace.service
"#,
    );
    let wants_dir = root.0.join("multi-user.target.wants");
    std::fs::create_dir_all(&wants_dir).unwrap();
    std::fs::write(wants_dir.join("replace.service"), "not a symlink").unwrap();
    std::fs::write(root.0.join("old-replace.service"), "not a symlink").unwrap();
    let mut manager = manager_with_unit_dir(&root);

    let created = manager.enable("replace.service").await.unwrap();

    assert_eq!(created.len(), 2);
    assert_eq!(link_target(&wants_dir.join("replace.service")), unit_path);
    assert_eq!(link_target(&root.0.join("old-replace.service")), unit_path);
}

#[tokio::test]
async fn disable_removes_existing_links_and_ignores_missing_links() {
    let root = temp_dir("disable");
    let unit_path = write_unit(
        &root,
        "remove.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
RequiredBy=boot.target
Alias=remove-alias.service
"#,
    );
    let wants_link = root.0.join("multi-user.target.wants/remove.service");
    let alias_link = root.0.join("remove-alias.service");
    std::fs::create_dir_all(wants_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&unit_path, &wants_link).unwrap();
    std::os::unix::fs::symlink(&unit_path, &alias_link).unwrap();
    let mut manager = manager_with_unit_dir(&root);

    let removed = manager.disable("remove.service").await.unwrap();

    assert_eq!(removed, vec![wants_link.clone(), alias_link.clone()]);
    assert!(!wants_link.exists());
    assert!(!alias_link.exists());
}

#[tokio::test]
async fn enable_reports_no_install_section_for_static_units() {
    let root = temp_dir("static");
    write_unit(
        &root,
        "static.service",
        r#"
[Service]
ExecStart=/bin/true
"#,
    );
    let mut manager = manager_with_unit_dir(&root);

    let err = manager.enable("static.service").await.unwrap_err();

    assert!(matches!(err, ManagerError::NoInstallSection(name) if name == "static.service"));
}

#[tokio::test]
async fn enable_skips_missing_also_units_but_keeps_primary_links() {
    let root = temp_dir("missing-also");
    write_unit(
        &root,
        "primary.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
Also=missing-helper.service
"#,
    );
    let mut manager = manager_with_unit_dir(&root);

    let created = manager.enable("primary.service").await.unwrap();

    assert_eq!(created, vec![root.0.join("multi-user.target.wants/primary.service")]);
}

#[tokio::test]
async fn is_enabled_reports_static_disabled_and_enabled_user_links() {
    let root = temp_dir("is-enabled");
    let enabled_path = write_unit(
        &root,
        "enabled.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
"#,
    );
    write_unit(
        &root,
        "disabled.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
WantedBy=multi-user.target
"#,
    );
    write_unit(
        &root,
        "static.service",
        r#"
[Service]
ExecStart=/bin/true
"#,
    );
    let wants_link = root.0.join("multi-user.target.wants/enabled.service");
    std::fs::create_dir_all(wants_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&enabled_path, &wants_link).unwrap();
    let mut manager = manager_with_unit_dir(&root);

    assert_eq!(manager.is_enabled("enabled").await.unwrap(), "enabled");
    assert_eq!(manager.is_enabled("disabled").await.unwrap(), "disabled");
    assert_eq!(manager.is_enabled("static").await.unwrap(), "static");
}

#[tokio::test]
async fn is_enabled_reports_alias_and_required_links_as_enabled() {
    let root = temp_dir("alias-required");
    let required_path = write_unit(
        &root,
        "required.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
RequiredBy=boot.target
"#,
    );
    let alias_path = write_unit(
        &root,
        "aliased.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
Alias=aliased-short.service
"#,
    );
    let required_link = root.0.join("boot.target.requires/required.service");
    std::fs::create_dir_all(required_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&required_path, &required_link).unwrap();
    std::os::unix::fs::symlink(&alias_path, root.0.join("aliased-short.service")).unwrap();
    let mut manager = manager_with_unit_dir(&root);

    assert_eq!(manager.is_enabled("required.service").await.unwrap(), "enabled");
    assert_eq!(manager.is_enabled("aliased.service").await.unwrap(), "enabled");
}
