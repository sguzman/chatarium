//! Explicit inference feature admission gates.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySlot {
    ImageInput,
    FileInput,
    FunctionTools,
    AdditionalTools,
    WebSearch,
    Reasoning,
    Verbosity,
    StructuredOutput,
}

impl CapabilitySlot {
    pub const ALL: [Self; 8] = [
        Self::ImageInput,
        Self::FileInput,
        Self::FunctionTools,
        Self::AdditionalTools,
        Self::WebSearch,
        Self::Reasoning,
        Self::Verbosity,
        Self::StructuredOutput,
    ];

    #[must_use]
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::ImageInput => "image_input",
            Self::FileInput => "file_input",
            Self::FunctionTools => "function_tools",
            Self::AdditionalTools => "additional_tools",
            Self::WebSearch => "web_search",
            Self::Reasoning => "reasoning",
            Self::Verbosity => "verbosity",
            Self::StructuredOutput => "structured_output",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ImageInput => "image input",
            Self::FileInput => "file input",
            Self::FunctionTools => "function tools",
            Self::AdditionalTools => "additional tools",
            Self::WebSearch => "web search",
            Self::Reasoning => "reasoning",
            Self::Verbosity => "verbosity",
            Self::StructuredOutput => "structured output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAdmissionState {
    Available,
    UnsupportedRoute,
    BlockedContract,
}

impl CapabilityAdmissionState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::UnsupportedRoute => "unsupported",
            Self::BlockedContract => "blocked · contract",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAdmission {
    pub slot: CapabilitySlot,
    pub state: CapabilityAdmissionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGate {
    pub admissions: Vec<CapabilityAdmission>,
}

impl CapabilityGate {
    #[must_use]
    pub fn from_contract(
        contract: Option<&LoadedContract>,
        profile_id: Option<&str>,
        model: Option<&str>,
    ) -> Self {
        let ready = match (contract, model) {
            (Some(contract), Some(model)) if contract.ready_for(profile_id, model) => {
                Some(contract)
            }
            _ => None,
        };

        let web_search_evidence = match (contract, model) {
            (Some(contract), Some(model))
                if contract.profile_id.as_deref() == profile_id && contract.model == model =>
            {
                Some(contract)
            }
            _ => None,
        };
        let admissions = CapabilitySlot::ALL
            .into_iter()
            .map(|slot| {
                // Only web search can be admitted by a focused probe without
                // making the rest of the incomplete contract appear ready.
                let evidence = if slot == CapabilitySlot::WebSearch {
                    web_search_evidence
                } else {
                    ready
                };
                let state = match evidence
                    .and_then(|contract| contract.capabilities.get(slot.contract_name()))
                    .map(String::as_str)
                {
                    Some("supported") => CapabilityAdmissionState::Available,
                    Some("unsupported_route") => CapabilityAdmissionState::UnsupportedRoute,
                    _ => CapabilityAdmissionState::BlockedContract,
                };
                CapabilityAdmission { slot, state }
            })
            .collect();

        Self { admissions }
    }

    #[must_use]
    pub fn allows(&self, slot: CapabilitySlot) -> bool {
        self.admissions.iter().any(|admission| {
            admission.slot == slot && admission.state == CapabilityAdmissionState::Available
        })
    }
}

