//! Read-only next-step guidance for manually managed MCP providers.
//!
//! This consumes already-replayed facts, never grants authority, records
//! events, attempts transport, or treats an advertised catalog as trusted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderWorkflowFacts {
    pub builtin: bool,
    pub endpoint_bound: bool,
    pub transport_configured: bool,
    pub activated: bool,
    pub source_addressable: bool,
    pub catalog_inspection_allowlisted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderWorkflowStep {
    Builtin,
    BindEndpoint,
    ConfigureTransport,
    Activate,
    AddressSource,
    ReadyForManualCall,
}

impl ProviderWorkflowStep {
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Builtin => "Builtin adapter",
            Self::BindEndpoint => "1 / 4 · Bind provider endpoint",
            Self::ConfigureTransport => "2 / 4 · Configure immutable stdio",
            Self::Activate => "3 / 4 · Activate provider",
            Self::AddressSource => "4 / 4 · Address source conversation",
            Self::ReadyForManualCall => "Configured · manual call review",
        }
    }

    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Builtin => "Builtin hello uses its own explicitly approved path; external stdio configuration does not apply.",
            Self::BindEndpoint => "Expand Provider registry and select Bind routing endpoint for this provider. No tool is executed.",
            Self::ConfigureTransport => "Expand External MCP stdio, enter a trusted canonical /usr/bin executable and the exact argv and allowed operations, then save the immutable configuration. No process starts.",
            Self::Activate => "Expand External MCP stdio and select Activate. This checks the executable; individual tool calls still need separate approval and Run.",
            Self::AddressSource => "Select or initialize a local conversation with an active routing endpoint before recording a tool call.",
            Self::ReadyForManualCall => "Enter an allowed operation and review its arguments, then Record call, explicitly Allow the route, review the activation-aware wire request, and separately Run once. None of these steps is automatic.",
        }
    }
}

pub const fn next_step(facts: ProviderWorkflowFacts) -> ProviderWorkflowStep {
    if facts.builtin {
        ProviderWorkflowStep::Builtin
    } else if !facts.endpoint_bound {
        ProviderWorkflowStep::BindEndpoint
    } else if !facts.transport_configured {
        ProviderWorkflowStep::ConfigureTransport
    } else if !facts.activated {
        ProviderWorkflowStep::Activate
    } else if !facts.source_addressable {
        ProviderWorkflowStep::AddressSource
    } else {
        ProviderWorkflowStep::ReadyForManualCall
    }
}

/// Optional inspection never changes a provider's readiness or permissions.
pub const fn catalog_note(facts: ProviderWorkflowFacts) -> &'static str {
    if facts.builtin || !facts.transport_configured {
        ""
    } else if facts.catalog_inspection_allowlisted {
        "Optional: Prepare approved tools/list inspection only drafts a request. Record, Allow, and Run each remain separate."
    } else {
        "Optional tools/list inspection is unavailable unless chatarium.internal.tools-list was included in the immutable allowlist before configuration."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ProviderWorkflowFacts {
        ProviderWorkflowFacts {
            builtin: false,
            endpoint_bound: true,
            transport_configured: true,
            activated: true,
            source_addressable: true,
            catalog_inspection_allowlisted: false,
        }
    }

    #[test]
    fn guidance_follows_prerequisite_order_without_changing_authority() {
        let mut f = facts();
        assert_eq!(next_step(f), ProviderWorkflowStep::ReadyForManualCall);
        f.source_addressable = false;
        assert_eq!(next_step(f), ProviderWorkflowStep::AddressSource);
        f.activated = false;
        assert_eq!(next_step(f), ProviderWorkflowStep::Activate);
        f.transport_configured = false;
        assert_eq!(next_step(f), ProviderWorkflowStep::ConfigureTransport);
        f.endpoint_bound = false;
        assert_eq!(next_step(f), ProviderWorkflowStep::BindEndpoint);
        f.builtin = true;
        assert_eq!(next_step(f), ProviderWorkflowStep::Builtin);
    }

    #[test]
    fn optional_catalog_does_not_skip_any_required_stage() {
        let mut f = facts();
        f.endpoint_bound = false;
        let before = next_step(f);
        f.catalog_inspection_allowlisted = true;
        assert_eq!(next_step(f), before);
        assert!(catalog_note(f).contains("Record, Allow, and Run"));
        f.transport_configured = false;
        assert!(catalog_note(f).is_empty());
    }

    #[test]
    fn guides_never_promise_automatic_execution_or_new_permission() {
        for step in [
            ProviderWorkflowStep::Builtin,
            ProviderWorkflowStep::BindEndpoint,
            ProviderWorkflowStep::ConfigureTransport,
            ProviderWorkflowStep::Activate,
            ProviderWorkflowStep::AddressSource,
            ProviderWorkflowStep::ReadyForManualCall,
        ] {
            assert!(!step.heading().is_empty());
            assert!(!step.instruction().is_empty());
            assert!(!step.instruction().contains("automatically execute"));
        }
    }
}
