# CDP capture harness bundle contract

This document defines the portable JSON boundary accepted by
`chatarium_recorder::sanitize_capture_bundle_bytes`. No CDP harness implementation or
prior bundle fixture exists in this checkout, so this is a proposed v1 interchange
contract, not a claim about an observed browser/client payload. The harness must not
silently change its emitted shape; update this contract and its fixtures when the
harness implementation becomes available.

## Shape

```json
{
  "format": "chatarium-cdp-capture-bundle",
  "version": 1,
  "captures": [
    {
      "id": "C01-send-text",
      "har": { "log": { "entries": [] } }
    }
  ]
}
```

The root `format`, integer `version`, and `captures` array are required. Every capture
must contain a HAR object with an array at `/log/entries`. Other properties are
allowed and preserved. The library rejects an unknown format/version or malformed
capture instead of guessing how to interpret it.

## Sanitization behavior

The library validates each nested HAR, then applies the same centralized sensitive
value redactor used by the HAR CLI to the entire bundle. This covers capture metadata
as well as HAR values. It preserves unknown fields and the established redaction
semantics. As with `sanitize-har`, syntactically valid JSON embedded in a string field
named `text` is parsed, sanitized, and serialized compactly; malformed/non-JSON text is
retained. This can change whitespace in embedded JSON and is existing sanitizer
behavior, not byte-preserving evidence handling.

The bundle operation is offline. It performs no browser control, network access,
authentication, or session handling. Successful sanitization is not approval to
publish: bundles can contain private conversation text and still require human review.
Keep raw bundles outside Git when they contain sensitive or private material.

## Uncertainty

There is no harness producer, versioned capture sample, or prior `docs/CAPTURE_HARNESS.md`
in the repository history available to establish a pre-existing portable format. The
shape above is therefore a library contract introduced for issue #2. Confirm it against
the eventual harness before treating it as empirically established.
