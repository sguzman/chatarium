# Schemas

Schemas here are **derived** from sanitized observations. They are conveniences for testing and implementation, not authoritative contracts from OpenAI.

Prefer evolution-tolerant parsing. Unknown fields should remain observable for diagnostics rather than being silently discarded at capture time.


## Evidence-scoped field classification

`field-classification.v1.json` records how Chatarium should compare currently observed fields without converting a small evidence set into a false compatibility promise.

Its vocabulary is intentionally not a binary stable/unstable flag:

- `structural_candidate` — observed protocol vocabulary or shape that current interpretation depends on;
- `ephemeral_instance` — per-run/per-message/per-conversation identity or timestamp data;
- `delivery_noise` — browser/local delivery details that are not protocol frame boundaries;
- `observation_count` — meaningful counts whose numeric value may vary across equivalent runs;
- `diagnostic` — recorder/parser/provenance metadata;
- `controlled_fixture` — exact content retained because a canonical experiment explicitly permits it;
- `unknown` — current evidence does not justify a stronger classification.

Every registry classification carries a rationale and the evidence revisions on which the registry is based. The corpus validator rejects unknown classification vocabulary and evidence references to snapshots that do not exist.
