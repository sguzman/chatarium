# Remote health and mirror cooldowns

The production mirror controller keeps principal intent separate from the health of the
first-party remote service. `START` and `RESUME` enable mirror intent, but capture is allowed
only after a bounded health check records `HEALTHY`. `PAUSE` records `MANUALLY PAUSED` and
`RECHECK REMOTE HEALTH` is a separate action; cooldown expiry only makes a later explicit
health check eligible and never starts capture by itself.

Health observations are append-only structural events. They contain only the classification,
timestamp, HTTP status when known, challenge flag, cooldown deadline, and consecutive failure
count. They do not contain response bodies, cookies, tokens, conversation text, or account
identifiers.

The state machine distinguishes authenticated HTTP 200, genuine unauthenticated results,
rate limiting, network instability, backend unavailability, and server challenges. A
challenge-marked HTTP 403 is `SERVER CHALLENGE`/unknown health, not logout and not a forced
re-login. The remote worker stops safely while local browsing and local archive reading remain
available. Cooldowns are deterministic and restart-safe; there is no polling retry storm.

The synthetic QA command `chatarium-qa remote-health-status` exercises transitions, journal
replay, cooldown expiry, queue independence, and the no-browser/no-remote invariant. Live
acceptance is intentionally separate and must not be inferred from this local proof.
