# Privacy-bounded diagnostic bundles

Diagnostic export is a two-step protocol-v1 workflow. `diagnostic.plan` returns the
exact redacted entry list, per-entry sizes, total uncompressed size, selected-image
flags, and a privacy warning. `diagnostic.export` succeeds only when the caller sets
`privacy_reviewed` and supplies the exact total from the reviewed plan; any intervening
content change forces a new review.

Bundles contain:

- bounded recent daemon diagnostic log records;
- redacted settings and the explicitly selected portable profile;
- current system, capture, and analysis metrics;
- up to eight region crops selected from a frozen preview.

The daemon never adds a full screenshot implicitly. Image entries can only be created
from element regions explicitly selected by the caller while a preview is frozen. The
GUI lists each selected crop and requires a visible confirmation before enabling its
export button. The CLI exposes the same contract:

```bash
yash-eventsctl diagnostic plan --profile-id <uuid> --element-id <uuid>
yash-eventsctl diagnostic export ./diagnostic.zip \
  --profile-id <uuid> --element-id <uuid> \
  --expected-total-bytes <reviewed-value> --privacy-reviewed
```

Recursive redaction removes fields whose names indicate tokens, secrets, passwords,
authorization, cookies, capture bindings, portal sessions, or local window IDs. Tests
place those values at multiple nesting levels and verify that neither keys nor values
enter the ZIP.

Default limits are 1,000 log records, eight crops, 4 MiB per entry, and 16 MiB total
uncompressed content. Crop names are generated from stable element IDs, crop bytes
must have a PNG signature, and archive creation uses a same-directory temporary file,
flush, sync, and atomic rename. Invalid images and resource-limit failures leave no
destination file.
