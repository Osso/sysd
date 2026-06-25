include!("path_watcher_impl.rs");

#[cfg(test)]
#[path = "path_watcher_impl_tests.rs"]
mod extra_tests;
