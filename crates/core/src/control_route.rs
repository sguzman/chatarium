//! Correlation between admitted worker controls and supervisory routes.
//!
//! A binding is identity/provenance only. It does not imply policy approval,
//! dispatch, delivery, execution, or worker lifecycle change.

use crate::control::ControlId;
use crate::routing::{RouteClass, RouteId, RouteRequest};

/// One admitted worker control correlated to the orchestration route that carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRouteBinding {
    control_id: ControlId,
    route_id: RouteId,
}

impl ControlRouteBinding {
    /// Bind an admitted control identity to an orchestration-class route.
    pub fn new(
        control_id: ControlId,
        route: &RouteRequest,
    ) -> Result<Self, ControlRouteBindingError> {
        if route.class != RouteClass::OrchestrationControl {
            return Err(ControlRouteBindingError::WrongRouteClass {
                route_id: route.id,
                actual: route.class,
            });
        }

        Ok(Self {
            control_id,
            route_id: route.id,
        })
    }

    /// Admitted control carried by the route.
    #[must_use]
    pub const fn control_id(self) -> ControlId {
        self.control_id
    }

    /// Route carrying the control.
    #[must_use]
    pub const fn route_id(self) -> RouteId {
        self.route_id
    }
}

/// Why a control cannot be correlated to a proposed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRouteBindingError {
    /// Worker controls may only travel on orchestration-control routes.
    WrongRouteClass {
        /// Proposed route identity.
        route_id: RouteId,
        /// Actual route class.
        actual: RouteClass,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::RouteEndpointId;

    const SOURCE: RouteEndpointId = RouteEndpointId::new(10);
    const DESTINATION: RouteEndpointId = RouteEndpointId::new(20);

    fn route(id: u64, class: RouteClass) -> RouteRequest {
        RouteRequest {
            id: RouteId::new(id),
            source: SOURCE,
            destination: DESTINATION,
            class,
        }
    }

    #[test]
    fn orchestration_route_can_carry_control() {
        let route = route(1, RouteClass::OrchestrationControl);
        let binding = ControlRouteBinding::new(ControlId::new(7), &route).unwrap();

        assert_eq!(binding.control_id(), ControlId::new(7));
        assert_eq!(binding.route_id(), RouteId::new(1));
    }

    #[test]
    fn other_route_classes_are_rejected() {
        for class in [RouteClass::SessionMessage, RouteClass::ToolCall] {
            let route = route(1, class);
            assert_eq!(
                ControlRouteBinding::new(ControlId::new(7), &route),
                Err(ControlRouteBindingError::WrongRouteClass {
                    route_id: RouteId::new(1),
                    actual: class,
                })
            );
        }
    }
}
