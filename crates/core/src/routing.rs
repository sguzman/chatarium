//! Pure domain gating for future Chatarium cross-session and tool routing.
//!
//! This module decides whether one already-identified route may leave Chatarium.
//! It contains no payload format, networking, persistence, UI, MCP, XML, or
//! ChatGPT-specific transport behavior.

/// Opaque identity for one routed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteId(u64);

impl RouteId {
    /// Construct a route identity from a caller-owned local value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque identity for a routing endpoint such as a session or tool adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteEndpointId(u64);

impl RouteEndpointId {
    /// Construct an endpoint identity from a caller-owned local value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Broad class of routed action.
///
/// Payload/wire representation belongs to later layers. In particular, this does
/// not define the future XML tool envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteClass {
    /// Ordinary content routed between conversation/session surfaces.
    SessionMessage,
    /// Master/worker lifecycle or other orchestration control traffic.
    OrchestrationControl,
    /// Request destined for an MCP/tool surface.
    ToolCall,
}

/// Identity and provenance boundary for one proposed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteRequest {
    /// Unique route identity.
    pub id: RouteId,
    /// Source endpoint.
    pub source: RouteEndpointId,
    /// Destination endpoint.
    pub destination: RouteEndpointId,
    /// Broad action class.
    pub class: RouteClass,
}

/// Policy requirement applied before user intervention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePolicy {
    /// The configured rule allows dispatch without asking the user.
    Allow,
    /// The configured rule forbids dispatch unless the user explicitly overrides it.
    Deny,
    /// Dispatch must wait for a user decision.
    RequireApproval,
}

/// Provenance for an allow/deny decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAuthority {
    /// Decision came from the configured automatic policy.
    Policy,
    /// Decision was made explicitly by the user.
    User,
}

/// Current pre/post-dispatch gate state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteGateState {
    /// No dispatch decision exists yet; user approval is required.
    PendingApproval,
    /// Dispatch is currently allowed.
    Allowed {
        /// Authority responsible for the current allow decision.
        by: DecisionAuthority,
    },
    /// Dispatch is currently forbidden.
    Denied {
        /// Authority responsible for the current deny decision.
        by: DecisionAuthority,
    },
    /// A one-shot dispatch permit has already been consumed.
    Dispatched {
        /// Authority that authorized the dispatch at the moment it left the gate.
        authorized_by: DecisionAuthority,
    },
}

impl RouteGateState {
    /// Whether this state can currently issue a dispatch permit.
    #[must_use]
    pub const fn allows_dispatch(self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    /// Whether the route is waiting for an explicit user decision.
    #[must_use]
    pub const fn requires_user_decision(self) -> bool {
        matches!(self, Self::PendingApproval)
    }

    /// Whether dispatch authorization has already been consumed.
    #[must_use]
    pub const fn is_dispatched(self) -> bool {
        matches!(self, Self::Dispatched { .. })
    }
}

/// Result of applying a user decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionOutcome {
    /// Gate state changed.
    Changed,
    /// The same effective user decision was already present.
    Unchanged,
}

/// Error while evaluating or consuming a route gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteGateError {
    /// Caller referenced a different route than the gate owns.
    RouteMismatch {
        /// Gate-owned route identity.
        expected: RouteId,
        /// Caller-provided route identity.
        received: RouteId,
    },
    /// User approval is still required.
    PendingApproval,
    /// The route is explicitly denied.
    Denied {
        /// Authority responsible for the denial.
        by: DecisionAuthority,
    },
    /// Dispatch authorization has already been consumed.
    AlreadyDispatched,
}

/// One-shot authorization to dispatch a specific route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchPermit {
    route: RouteRequest,
    authorized_by: DecisionAuthority,
}

impl DispatchPermit {
    /// Route identity authorized by this permit.
    #[must_use]
    pub const fn route_id(self) -> RouteId {
        self.route.id
    }

    /// Source endpoint.
    #[must_use]
    pub const fn source(self) -> RouteEndpointId {
        self.route.source
    }

    /// Destination endpoint.
    #[must_use]
    pub const fn destination(self) -> RouteEndpointId {
        self.route.destination
    }

    /// Routed action class.
    #[must_use]
    pub const fn class(self) -> RouteClass {
        self.route.class
    }

    /// Authority that permitted dispatch.
    #[must_use]
    pub const fn authorized_by(self) -> DecisionAuthority {
        self.authorized_by
    }
}

/// Policy gate for one proposed route.
///
/// The gate is one-shot. An allowed route can produce exactly one dispatch
/// permit, after which the gate preserves that historical fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteGate {
    request: RouteRequest,
    state: RouteGateState,
}

impl RouteGate {
    /// Create a route gate from the current automatic policy requirement.
    #[must_use]
    pub const fn new(request: RouteRequest, policy: RoutePolicy) -> Self {
        let state = match policy {
            RoutePolicy::Allow => RouteGateState::Allowed {
                by: DecisionAuthority::Policy,
            },
            RoutePolicy::Deny => RouteGateState::Denied {
                by: DecisionAuthority::Policy,
            },
            RoutePolicy::RequireApproval => RouteGateState::PendingApproval,
        };
        Self { request, state }
    }

    /// Route identity/provenance being gated.
    #[must_use]
    pub const fn request(self) -> RouteRequest {
        self.request
    }

    /// Current gate state.
    #[must_use]
    pub const fn state(self) -> RouteGateState {
        self.state
    }

    /// Explicitly allow the route as the user.
    ///
    /// This may override an automatic policy denial before dispatch. It cannot
    /// rewrite a route after dispatch has already occurred.
    pub fn user_allow(&mut self) -> Result<DecisionOutcome, RouteGateError> {
        match self.state {
            RouteGateState::Dispatched { .. } => Err(RouteGateError::AlreadyDispatched),
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            } => Ok(DecisionOutcome::Unchanged),
            RouteGateState::PendingApproval
            | RouteGateState::Allowed {
                by: DecisionAuthority::Policy,
            }
            | RouteGateState::Denied { .. } => {
                self.state = RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                };
                Ok(DecisionOutcome::Changed)
            }
        }
    }

    /// Explicitly forbid the route as the user.
    ///
    /// User denial can veto an automatic policy allow before dispatch. A route
    /// that already dispatched remains historically dispatched.
    pub fn user_deny(&mut self) -> Result<DecisionOutcome, RouteGateError> {
        match self.state {
            RouteGateState::Dispatched { .. } => Err(RouteGateError::AlreadyDispatched),
            RouteGateState::Denied {
                by: DecisionAuthority::User,
            } => Ok(DecisionOutcome::Unchanged),
            RouteGateState::PendingApproval
            | RouteGateState::Allowed { .. }
            | RouteGateState::Denied {
                by: DecisionAuthority::Policy,
            } => {
                self.state = RouteGateState::Denied {
                    by: DecisionAuthority::User,
                };
                Ok(DecisionOutcome::Changed)
            }
        }
    }

    /// Consume the current allow decision and issue the only dispatch permit.
    ///
    /// The expected route identity prevents a stale caller from consuming the
    /// wrong gate.
    pub fn authorize_dispatch(
        &mut self,
        expected_route: RouteId,
    ) -> Result<DispatchPermit, RouteGateError> {
        if expected_route != self.request.id {
            return Err(RouteGateError::RouteMismatch {
                expected: self.request.id,
                received: expected_route,
            });
        }

        let authority = match self.state {
            RouteGateState::PendingApproval => return Err(RouteGateError::PendingApproval),
            RouteGateState::Denied { by } => return Err(RouteGateError::Denied { by }),
            RouteGateState::Dispatched { .. } => return Err(RouteGateError::AlreadyDispatched),
            RouteGateState::Allowed { by } => by,
        };

        self.state = RouteGateState::Dispatched {
            authorized_by: authority,
        };
        Ok(DispatchPermit {
            route: self.request,
            authorized_by: authority,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: RouteEndpointId = RouteEndpointId::new(10);
    const WORKER: RouteEndpointId = RouteEndpointId::new(20);
    const TOOL: RouteEndpointId = RouteEndpointId::new(30);
    const R1: RouteId = RouteId::new(1);
    const R2: RouteId = RouteId::new(2);

    fn request(id: RouteId, destination: RouteEndpointId, class: RouteClass) -> RouteRequest {
        RouteRequest {
            id,
            source: MASTER,
            destination,
            class,
        }
    }

    #[test]
    fn auto_allow_issues_exactly_one_dispatch_permit() {
        let route = request(R1, WORKER, RouteClass::SessionMessage);
        let mut gate = RouteGate::new(route, RoutePolicy::Allow);

        let permit = gate.authorize_dispatch(R1).unwrap();
        assert_eq!(permit.route_id(), R1);
        assert_eq!(permit.source(), MASTER);
        assert_eq!(permit.destination(), WORKER);
        assert_eq!(permit.class(), RouteClass::SessionMessage);
        assert_eq!(permit.authorized_by(), DecisionAuthority::Policy);
        assert_eq!(
            gate.state(),
            RouteGateState::Dispatched {
                authorized_by: DecisionAuthority::Policy,
            }
        );
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::AlreadyDispatched)
        );
    }

    #[test]
    fn auto_deny_cannot_dispatch() {
        let mut gate = RouteGate::new(
            request(R1, TOOL, RouteClass::ToolCall),
            RoutePolicy::Deny,
        );
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::Denied {
                by: DecisionAuthority::Policy,
            })
        );
        assert_eq!(
            gate.state(),
            RouteGateState::Denied {
                by: DecisionAuthority::Policy,
            }
        );
    }

    #[test]
    fn approval_required_cannot_dispatch_while_pending() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::OrchestrationControl),
            RoutePolicy::RequireApproval,
        );
        assert!(gate.state().requires_user_decision());
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::PendingApproval)
        );
    }

    #[test]
    fn user_approval_unlocks_pending_route_with_user_provenance() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::OrchestrationControl),
            RoutePolicy::RequireApproval,
        );
        assert_eq!(gate.user_allow(), Ok(DecisionOutcome::Changed));
        assert_eq!(
            gate.state(),
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            }
        );

        let permit = gate.authorize_dispatch(R1).unwrap();
        assert_eq!(permit.authorized_by(), DecisionAuthority::User);
    }

    #[test]
    fn user_denial_keeps_pending_route_blocked() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::OrchestrationControl),
            RoutePolicy::RequireApproval,
        );
        assert_eq!(gate.user_deny(), Ok(DecisionOutcome::Changed));
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::Denied {
                by: DecisionAuthority::User,
            })
        );
    }

    #[test]
    fn user_can_veto_auto_allowed_route_before_dispatch() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::Allow,
        );
        assert!(gate.state().allows_dispatch());
        assert_eq!(gate.user_deny(), Ok(DecisionOutcome::Changed));
        assert_eq!(
            gate.state(),
            RouteGateState::Denied {
                by: DecisionAuthority::User,
            }
        );
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::Denied {
                by: DecisionAuthority::User,
            })
        );
    }

    #[test]
    fn user_can_explicitly_override_policy_denial() {
        let mut gate = RouteGate::new(
            request(R1, TOOL, RouteClass::ToolCall),
            RoutePolicy::Deny,
        );
        assert_eq!(gate.user_allow(), Ok(DecisionOutcome::Changed));
        let permit = gate.authorize_dispatch(R1).unwrap();
        assert_eq!(permit.authorized_by(), DecisionAuthority::User);
    }

    #[test]
    fn master_origin_has_no_special_dispatch_authority() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::OrchestrationControl),
            RoutePolicy::RequireApproval,
        );
        assert_eq!(gate.request().source, MASTER);
        assert_eq!(
            gate.authorize_dispatch(R1),
            Err(RouteGateError::PendingApproval)
        );
    }

    #[test]
    fn stale_route_identity_cannot_consume_another_gate() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::Allow,
        );
        assert_eq!(
            gate.authorize_dispatch(R2),
            Err(RouteGateError::RouteMismatch {
                expected: R1,
                received: R2,
            })
        );
        assert!(gate.state().allows_dispatch());
        assert!(gate.authorize_dispatch(R1).is_ok());
    }

    #[test]
    fn decision_provenance_remains_observable() {
        let policy_allowed = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::Allow,
        );
        assert_eq!(
            policy_allowed.state(),
            RouteGateState::Allowed {
                by: DecisionAuthority::Policy,
            }
        );

        let mut user_allowed = RouteGate::new(
            request(R2, TOOL, RouteClass::ToolCall),
            RoutePolicy::RequireApproval,
        );
        user_allowed.user_allow().unwrap();
        assert_eq!(
            user_allowed.state(),
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            }
        );
    }

    #[test]
    fn dispatched_state_is_distinct_from_merely_allowed() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::Allow,
        );
        assert_eq!(
            gate.state(),
            RouteGateState::Allowed {
                by: DecisionAuthority::Policy,
            }
        );
        gate.authorize_dispatch(R1).unwrap();
        assert!(gate.state().is_dispatched());
        assert!(!gate.state().allows_dispatch());
    }

    #[test]
    fn user_decision_after_dispatch_does_not_rewrite_history() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::Allow,
        );
        gate.authorize_dispatch(R1).unwrap();
        let dispatched = gate.state();

        assert_eq!(gate.user_deny(), Err(RouteGateError::AlreadyDispatched));
        assert_eq!(gate.user_allow(), Err(RouteGateError::AlreadyDispatched));
        assert_eq!(gate.state(), dispatched);
    }

    #[test]
    fn duplicate_same_user_decision_is_idempotent() {
        let mut gate = RouteGate::new(
            request(R1, WORKER, RouteClass::SessionMessage),
            RoutePolicy::RequireApproval,
        );
        assert_eq!(gate.user_deny(), Ok(DecisionOutcome::Changed));
        assert_eq!(gate.user_deny(), Ok(DecisionOutcome::Unchanged));

        assert_eq!(gate.user_allow(), Ok(DecisionOutcome::Changed));
        assert_eq!(gate.user_allow(), Ok(DecisionOutcome::Unchanged));
    }
}
