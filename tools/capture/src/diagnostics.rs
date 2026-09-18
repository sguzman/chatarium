//! Read-only Windows evidence collected when the local DevTools listener is not ready.

use serde::Serialize;
use std::fmt;
use std::net::IpAddr;

/// One TCP record matching the harness-selected DevTools port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListenerRecord {
    /// Local address reported by the operating system.
    pub local_address: IpAddr,
    /// Local TCP port.
    pub local_port: u16,
    /// TCP state reported by the operating system.
    pub state: String,
    /// Owning process ID, when exposed by the operating system.
    pub owning_pid: Option<u32>,
}

/// State observed for one Edge remote-debugging policy registry value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum PolicyState {
    /// No registry value was configured.
    NotConfigured,
    /// The DWORD value was nonzero.
    Enabled,
    /// The DWORD value was zero.
    Disabled,
    /// The value could not be read or had an unexpected type/value.
    Unreadable(String),
}

impl fmt::Display for PolicyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => formatter.write_str("not configured"),
            Self::Enabled => formatter.write_str("enabled"),
            Self::Disabled => formatter.write_str("disabled"),
            Self::Unreadable(reason) => write!(formatter, "unreadable ({reason})"),
        }
    }
}

/// Read-only observations of the machine and user Edge policy values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteDebuggingPolicy {
    /// HKLM policy value.
    pub machine: PolicyState,
    /// HKCU policy value.
    pub user: PolicyState,
}

impl RemoteDebuggingPolicy {
    /// Whether the inspected registry values establish an unambiguous disabled state.
    ///
    /// Conflicting or unreadable values are diagnostic evidence, not proof that the effective
    /// Edge policy disables remote debugging, so startup must not short-circuit on them.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(
            (&self.machine, &self.user),
            (PolicyState::Disabled, PolicyState::Disabled)
                | (PolicyState::Disabled, PolicyState::NotConfigured)
                | (PolicyState::NotConfigured, PolicyState::Disabled)
        )
    }

    /// Short diagnostic state, preserving conflict/unknown information.
    #[must_use]
    pub fn summary(&self) -> String {
        match (&self.machine, &self.user) {
            (PolicyState::NotConfigured, PolicyState::NotConfigured) => "not configured".to_owned(),
            (PolicyState::Disabled, PolicyState::Disabled)
            | (PolicyState::Disabled, PolicyState::NotConfigured)
            | (PolicyState::NotConfigured, PolicyState::Disabled) => "disabled".to_owned(),
            (PolicyState::Enabled, PolicyState::Enabled)
            | (PolicyState::Enabled, PolicyState::NotConfigured)
            | (PolicyState::NotConfigured, PolicyState::Enabled) => "enabled".to_owned(),
            (PolicyState::Unreadable(_), _) | (_, PolicyState::Unreadable(_)) => {
                "unreadable/error".to_owned()
            }
            _ => "conflict/unknown".to_owned(),
        }
    }
}

/// Relationship between a listener process and the launched Edge process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerRelation {
    /// Listener belongs to the process Chatarium launched.
    Same,
    /// Listener belongs to a descendant of the launched process.
    Descendant,
    /// Listener belongs to a process outside the launched process tree.
    Different,
    /// Ownership or process ancestry could not be established.
    Unknown,
}

/// Mockable Windows evidence boundary; implementations must inspect only the selected port.
pub trait EdgeDiagnostics: Send + Sync {
    /// Return matching TCP records for the selected DevTools port.
    fn listeners(&self, port: u16) -> Result<Vec<ListenerRecord>, String>;
    /// Read HKLM/HKCU RemoteDebuggingAllowed values without modifying either hive.
    fn remote_debugging_policy(&self) -> RemoteDebuggingPolicy;
    /// Relate a listener-owning PID to the process launched by this harness.
    fn owner_relation(&self, owner_pid: u32, launched_pid: u32) -> OwnerRelation;
}

/// Native Windows implementation of the read-only diagnostic boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEdgeDiagnostics;

impl EdgeDiagnostics for SystemEdgeDiagnostics {
    fn listeners(&self, port: u16) -> Result<Vec<ListenerRecord>, String> {
        #[cfg(windows)]
        {
            use netstat2::{
                AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, get_sockets_info,
            };

            let sockets = get_sockets_info(
                AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
                ProtocolFlags::TCP,
            )
            .map_err(|error| format!("read Windows TCP table: {error}"))?;
            Ok(sockets
                .into_iter()
                .filter_map(|socket| {
                    let ProtocolSocketInfo::Tcp(tcp) = socket.protocol_socket_info else {
                        return None;
                    };
                    (tcp.local_port == port).then(|| ListenerRecord {
                        local_address: tcp.local_addr,
                        local_port: tcp.local_port,
                        state: if tcp.state == TcpState::Listen {
                            "LISTEN".to_owned()
                        } else {
                            tcp.state.to_string()
                        },
                        owning_pid: socket.associated_pids.first().copied(),
                    })
                })
                .collect())
        }
        #[cfg(not(windows))]
        {
            let _ = port;
            Err("native Windows TCP table is unavailable on this platform".to_owned())
        }
    }

    fn remote_debugging_policy(&self) -> RemoteDebuggingPolicy {
        #[cfg(windows)]
        {
            use winreg::RegKey;
            use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

            fn read(root: winreg::HKEY) -> PolicyState {
                let root = RegKey::predef(root);
                let path = r"SOFTWARE\Policies\Microsoft\Edge";
                let key = match root.open_subkey(path) {
                    Ok(key) => key,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return PolicyState::NotConfigured;
                    }
                    Err(error) => return PolicyState::Unreadable(error.to_string()),
                };
                match key.get_value::<u32, _>("RemoteDebuggingAllowed") {
                    Ok(0) => PolicyState::Disabled,
                    Ok(_) => PolicyState::Enabled,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        PolicyState::NotConfigured
                    }
                    Err(error) => PolicyState::Unreadable(error.to_string()),
                }
            }

            RemoteDebuggingPolicy {
                machine: read(HKEY_LOCAL_MACHINE),
                user: read(HKEY_CURRENT_USER),
            }
        }
        #[cfg(not(windows))]
        {
            RemoteDebuggingPolicy {
                machine: PolicyState::Unreadable("Windows registry unavailable".to_owned()),
                user: PolicyState::Unreadable("Windows registry unavailable".to_owned()),
            }
        }
    }

    fn owner_relation(&self, owner_pid: u32, launched_pid: u32) -> OwnerRelation {
        #[cfg(windows)]
        {
            use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

            if owner_pid == launched_pid {
                return OwnerRelation::Same;
            }
            let mut system = System::new();
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            let mut current = Pid::from_u32(owner_pid);
            let mut visited = std::collections::HashSet::new();
            for _ in 0..1024 {
                if !visited.insert(current) {
                    return OwnerRelation::Unknown;
                }
                let Some(process) = system.process(current) else {
                    return OwnerRelation::Unknown;
                };
                let Some(parent) = process.parent() else {
                    return OwnerRelation::Different;
                };
                if parent.as_u32() == launched_pid {
                    return OwnerRelation::Descendant;
                }
                current = parent;
            }
            OwnerRelation::Unknown
        }
        #[cfg(not(windows))]
        {
            let _ = (owner_pid, launched_pid);
            OwnerRelation::Unknown
        }
    }
}

/// Compare listener ownership using an injectable parent map.
#[must_use]
pub fn relation_from_parent_map(
    owner_pid: u32,
    launched_pid: u32,
    parents: &std::collections::HashMap<u32, Option<u32>>,
) -> OwnerRelation {
    if owner_pid == launched_pid {
        return OwnerRelation::Same;
    }
    let mut current = owner_pid;
    let mut visited = std::collections::HashSet::new();
    for _ in 0..1024 {
        if !visited.insert(current) {
            return OwnerRelation::Unknown;
        }
        let Some(parent) = parents.get(&current) else {
            return OwnerRelation::Unknown;
        };
        match parent {
            Some(parent) if *parent == launched_pid => return OwnerRelation::Descendant,
            Some(parent) => current = *parent,
            None => return OwnerRelation::Different,
        }
    }
    OwnerRelation::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn policy_state_summary_preserves_all_required_states() {
        assert_eq!(
            RemoteDebuggingPolicy {
                machine: PolicyState::NotConfigured,
                user: PolicyState::NotConfigured,
            }
            .summary(),
            "not configured"
        );
        assert_eq!(
            RemoteDebuggingPolicy {
                machine: PolicyState::Enabled,
                user: PolicyState::NotConfigured,
            }
            .summary(),
            "enabled"
        );
        let conflicting = RemoteDebuggingPolicy {
            machine: PolicyState::Disabled,
            user: PolicyState::Enabled,
        };
        assert_eq!(conflicting.summary(), "conflict/unknown");
        assert!(!conflicting.is_disabled());

        let unambiguous_disabled = RemoteDebuggingPolicy {
            machine: PolicyState::Disabled,
            user: PolicyState::NotConfigured,
        };
        assert_eq!(unambiguous_disabled.summary(), "disabled");
        assert!(unambiguous_disabled.is_disabled());
        assert_eq!(
            RemoteDebuggingPolicy {
                machine: PolicyState::Unreadable("denied".to_owned()),
                user: PolicyState::NotConfigured,
            }
            .summary(),
            "unreadable/error"
        );
    }

    #[test]
    fn process_owner_relationship_is_explicit_and_cycle_safe() {
        assert_eq!(
            relation_from_parent_map(100, 100, &HashMap::new()),
            OwnerRelation::Same
        );
        assert_eq!(
            relation_from_parent_map(
                102,
                100,
                &HashMap::from([(102, Some(101)), (101, Some(100))])
            ),
            OwnerRelation::Descendant
        );
        assert_eq!(
            relation_from_parent_map(200, 100, &HashMap::from([(200, None)])),
            OwnerRelation::Different
        );
        assert_eq!(
            relation_from_parent_map(200, 100, &HashMap::new()),
            OwnerRelation::Unknown
        );
        assert_eq!(
            relation_from_parent_map(
                201,
                100,
                &HashMap::from([(201, Some(202)), (202, Some(201))])
            ),
            OwnerRelation::Unknown
        );
    }
}
