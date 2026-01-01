//! Virtualization detection
//!
//! Detects container and VM environments for ConditionVirtualization=.

/// Detected virtualization type
#[derive(Debug, Clone, PartialEq)]
pub enum VirtualizationType {
    // Containers
    Docker,
    Podman,
    Lxc,
    Lxd,
    SystemdNspawn,
    Container, // Generic container

    // Virtual machines
    Qemu,
    VirtualBox,
    VMware,
    Xen,
    HyperV,
    Bochs,
    Vm, // Generic VM
}

impl VirtualizationType {
    /// Check if this is a container type
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            Self::Docker
                | Self::Podman
                | Self::Lxc
                | Self::Lxd
                | Self::SystemdNspawn
                | Self::Container
        )
    }

    /// Check if this is a VM type
    pub fn is_vm(&self) -> bool {
        matches!(
            self,
            Self::Qemu
                | Self::VirtualBox
                | Self::VMware
                | Self::Xen
                | Self::HyperV
                | Self::Bochs
                | Self::Vm
        )
    }

    /// Check if this matches a specific type name
    pub fn matches(&self, name: &str) -> bool {
        let name_lower = name.to_lowercase();
        match self {
            Self::Docker => name_lower == "docker",
            Self::Podman => name_lower == "podman",
            Self::Lxc => name_lower == "lxc",
            Self::Lxd => name_lower == "lxd" || name_lower == "lxc-libvirt",
            Self::SystemdNspawn => name_lower == "systemd-nspawn",
            Self::Container => name_lower == "container",
            Self::Qemu => name_lower == "qemu" || name_lower == "kvm",
            Self::VirtualBox => name_lower == "oracle" || name_lower == "virtualbox",
            Self::VMware => name_lower == "vmware",
            Self::Xen => name_lower == "xen",
            Self::HyperV => name_lower == "microsoft" || name_lower == "hyper-v",
            Self::Bochs => name_lower == "bochs",
            Self::Vm => name_lower == "vm",
        }
    }

    /// Parse from container= environment variable
    pub fn from_container_env(val: &str) -> Self {
        match val.to_lowercase().as_str() {
            "docker" => Self::Docker,
            "podman" => Self::Podman,
            "lxc" => Self::Lxc,
            "lxd" | "lxc-libvirt" => Self::Lxd,
            "systemd-nspawn" => Self::SystemdNspawn,
            _ => Self::Container,
        }
    }
}
