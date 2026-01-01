//! M19: DynamicUser= - Ephemeral UID/GID allocation
//!
//! Allocates UIDs from systemd's dynamic user range (61184-65519).
//! These are ephemeral: allocated on service start, freed on stop.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Dynamic user range (systemd uses 61184-65519)
/// See: https://systemd.io/UIDS-GIDS/
const DYNAMIC_UID_MIN: u32 = 61184;
const DYNAMIC_UID_MAX: u32 = 65519;

/// Manages ephemeral UID/GID allocation for DynamicUser= services
#[derive(Clone)]
pub struct DynamicUserManager {
    inner: Arc<Mutex<DynamicUserManagerInner>>,
}

struct DynamicUserManagerInner {
    /// Currently allocated UIDs (UID -> service name)
    allocated: HashSet<u32>,
    /// Next UID to try allocating
    next_uid: u32,
}

impl Default for DynamicUserManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicUserManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DynamicUserManagerInner {
                allocated: HashSet::new(),
                next_uid: DYNAMIC_UID_MIN,
            })),
        }
    }

    /// Allocate a dynamic UID/GID pair for a service
    /// Returns (uid, gid) - for simplicity, uid == gid
    pub fn allocate(&self, service_name: &str) -> Result<(u32, u32), DynamicUserError> {
        let mut inner = self.inner.lock().unwrap();

        // Find an available UID
        let start = inner.next_uid;
        let mut uid = start;

        loop {
            if !inner.allocated.contains(&uid) {
                // Found an available UID
                inner.allocated.insert(uid);
                inner.next_uid = if uid >= DYNAMIC_UID_MAX {
                    DYNAMIC_UID_MIN
                } else {
                    uid + 1
                };

                log::debug!(
                    "Allocated dynamic UID/GID {} for service {}",
                    uid,
                    service_name
                );
                return Ok((uid, uid)); // UID == GID for dynamic users
            }

            // Try next UID
            uid = if uid >= DYNAMIC_UID_MAX {
                DYNAMIC_UID_MIN
            } else {
                uid + 1
            };

            // Check if we've wrapped around
            if uid == start {
                return Err(DynamicUserError::PoolExhausted);
            }
        }
    }

    /// Release a dynamic UID/GID when service stops
    pub fn release(&self, uid: u32) {
        let mut inner = self.inner.lock().unwrap();
        if inner.allocated.remove(&uid) {
            log::debug!("Released dynamic UID/GID {}", uid);
        }
    }

    /// Check if a UID is in the dynamic range
    #[allow(dead_code)] // Used in tests, public API for future use
    pub fn is_dynamic_uid(uid: u32) -> bool {
        uid >= DYNAMIC_UID_MIN && uid <= DYNAMIC_UID_MAX
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DynamicUserError {
    #[error("Dynamic user pool exhausted (all UIDs in range {}-{} in use)", DYNAMIC_UID_MIN, DYNAMIC_UID_MAX)]
    PoolExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_and_release() {
        let mgr = DynamicUserManager::new();

        let (uid1, gid1) = mgr.allocate("test1.service").unwrap();
        assert!(DynamicUserManager::is_dynamic_uid(uid1));
        assert_eq!(uid1, gid1);

        let (uid2, _) = mgr.allocate("test2.service").unwrap();
        assert_ne!(uid1, uid2);

        mgr.release(uid1);

        // Should be able to allocate again
        let (uid3, _) = mgr.allocate("test3.service").unwrap();
        // uid3 might reuse uid1 eventually, but not necessarily immediately
        assert!(DynamicUserManager::is_dynamic_uid(uid3));
    }

    #[test]
    fn test_is_dynamic_uid() {
        assert!(!DynamicUserManager::is_dynamic_uid(0));
        assert!(!DynamicUserManager::is_dynamic_uid(1000));
        assert!(!DynamicUserManager::is_dynamic_uid(61183));
        assert!(DynamicUserManager::is_dynamic_uid(61184));
        assert!(DynamicUserManager::is_dynamic_uid(65000));
        assert!(DynamicUserManager::is_dynamic_uid(65519));
        assert!(!DynamicUserManager::is_dynamic_uid(65520));
    }
}
