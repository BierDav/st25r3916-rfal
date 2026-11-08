# Local modifications to ST's RFAL v4.0.2

Every change is marked in the C sources with a `/* st25r3916-rfal: ... */`
comment. All of them are **disabled by default**, so a default build behaves
exactly like upstream RFAL. They are enabled by Cargo features, which
`build.rs` turns into C defines (defaults to `0` live in
`src/c/rfal_platform.h`).

| Cargo feature | C define | File | Change |
|---|---|---|---|
| `apple-ecp` | `RFAL_FEATURE_APPLE_ECP` | `src/rfal_nfc.c`, `rfalNfcPollTechDetection()` | After NFC-A technology detection found no device, transmit an Apple Enhanced Contactless Polling (ECP v1) "VAS only" frame (`6A 01 00 00 02` + CRC_A `E4 D2`) so iPhones nearby bring up Apple Wallet. Transmit-only (no receive buffer, `RFAL_FWT_NONE`): blocks only for the frame duration (~0.7 ms); errors are ignored. |
| `force-wupa` | `RFAL_FEATURE_NFCA_FORCE_WUPA` | `src/rfal_nfca.c`, `rfalNfcaPollerStartTechnologyDetection()` | Always send WUPA instead of REQA (upstream: WUPA only in EMV mode), so devices in HALT state also answer. Not NFC Forum compliant. |
| `poll-keep-field-on` | `RFAL_FEATURE_POLL_KEEP_FIELD_ON` | `src/rfal_nfc.c`, `rfalNfcWorker()` (`POLL_TECHDETECT` and `LISTEN_TECHDETECT` states) | When no listen technologies are configured (`lmMask == 0`), keep the RF field on between discovery rounds instead of switching it off. Not NFC Forum compliant (no field-off/reset time between rounds). |

Files not built: `src/st25r3916/st25r3916_aat.c` (antenna auto-tuning, needs
`<stdlib.h>` and is not exposed by the crate).

History: these patches were first applied unconditionally (commit `6c4912a`),
which also removed RFAL's `POLL_SELECT` multi-device notification, the
`START_DISCOVERY` notification after a `LISTEN_TECHDETECT` timeout and the
guard-time restart in NFC-A detection. Those removals have been reverted; the
upstream behaviour is restored unconditionally.
