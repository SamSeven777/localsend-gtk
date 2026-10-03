# Familiarity with official LocalSend

The target is that an existing LocalSend user can recognize the app and complete
the same tasks without learning a different workflow. Pixel-identical rendering
is not required; typographic hierarchy, color roles, names, action order and feedback
are the reference. GTK4/Wayland remains the implementation, with font family
selection and language fallback delegated to the system's Fontconfig configuration.

History/cancellation reference: official [LocalSend](https://github.com/localsend/localsend) revision
[`f9b0e361052d31a4fddd26c6b4e84310d566d828`](https://github.com/localsend/localsend/commit/f9b0e361052d31a4fddd26c6b4e84310d566d828).
Typography/theme reference: official LocalSend revision
[`033d97511d1980db5283914f8a726f74d5fb7c17`](https://github.com/localsend/localsend/commit/033d97511d1980db5283914f8a726f74d5fb7c17).
Theme reference packages: Dart material_color_utilities 0.13.0, Yaru 10.2.0.

| Area | Current implementation | Remaining work |
| --- | --- | --- |
| Fonts | Generic sans-serif resolved through Pango/Fontconfig, including language fallback; no bundled or hardcoded concrete font families, including Yaru mode; familiar text sizes and weights | Rendering varies with installed fonts and Fontconfig settings; desktop text sizes are not inherited in full |
| Language | Live English, Simplified/Traditional Chinese, Japanese, Korean, German, French, Spanish and Russian; official terminology, Russian counted forms, named templates and explicit text bindings preserve user content and transfers | Additional upstream locales and some low-level error diagnostics |
| Themes | Light/dark, LocalSend, OLED, Yaru, custom HCT/TonalSpot and XDG portal system accent; runtime portal changes; distinct card, input, dialog and notification roles; independent Dart color vectors | Real GNOME/KDE portal switching and a complete widget-by-widget comparison |
| Navigation | Receive / Send / Settings; desktop sidebar, compact rail and narrow bottom bar (with optional configurable compact rail) | Some secondary screens use GTK dialogs |
| Selection | File / Folder / Text / Paste; selected thumbnails and Edit / Add; edit text, open files, delete; deferred target actions resume after selection | More media-specific thumbnails and upstream platform-only pickers |
| Single send | First tap enters Waiting for response; challenged PIN; per-file outcomes/bytes, total accepted bytes, speed, elapsed/remaining; distinct refusal/busy/rate-limit feedback; Continue/Cancel confirmation; Done; successful unchanged selection clears | Larger real-world transfer verification and remaining presentation details |
| Multiple send | Tapping each device starts an independent transfer; progress and cancellation per device; selection remains; cancellation confirms only transfers selected when the dialog opened | Exact upstream tile layout and retry presentation |
| Receive | Per-file acceptance and cancellation confirmation; session/per-file progress, counts, bytes, speed, elapsed/remaining, status, completed-file opening and Done; authenticated Favorites Quick save; inline messages always require Copy / Open / Close | Exact Flutter ProgressPage layout, completion countdown, taskbar/wake feedback, thumbnails and richer failure/retry presentation |
| History | Received files/messages with sender and time; Open file / Show in folder / Information / Delete from history; confirmed Delete history; Save to history defaults on; legacy entries are preserved on load; removing history keeps received files | Larger history and real desktop file-manager verification; old plain-text entries cannot recover missing metadata |
| Favorites / address | IP Address (IPv4/IPv6); Confirm / Cancel; offline editing preserves pinned identity; delete confirmation; editor returns to Favorites | Full alignment of less common connection settings |
| Settings | General/Receive/Send/Network categories; live language/theme/color controls; Save to history; Start/Restart/Stop and pending-restart hint; persistent network/desktop preferences | Remaining advanced settings and their exact upstream behavior |
| Browser sharing | QR/link, copy, approval, progress and stopping | Official browser protocol, encryption/PIN options and persistent request list; current temporary HTTP service is explicitly described |

Unreadable history is left untouched: saved-history writes and deletions are blocked,
while new receipts can be viewed for the current session. Save to history controls new
receipts, including full incoming message text; switching it off does not clear old entries.

The automated suite covers history, cancellation, authenticated Favorites Quick save,
X.509 identity checks, multi-file progress, 14 browser tests, formatting, Clippy, and
the separate Wayland interaction fixture in both ordinary and high-contrast modes.
The latest local run passed 105 application tests (2 environment tests run separately),
175 vendored protocol tests (1 external-network test ignored), all 14 browser tests,
isolated tray interactions, and both Wayland modes.
Validation combines pure palette vectors, real loopback HTTP/HTTPS protocol tests and GTK
interaction/rendering on a private Weston compositor. Tests cover ordinary
and high-contrast appearance, desktop through 360×540 layouts, selection continuation,
independent transfers, challenged PIN entry, message copying, locale changes during
active transfers, retained combo choices, receiver lifecycle, history metadata and
message actions at 360×540, multi-file receive details, certificate-fingerprint device
deduplication, overlapping native/browser session identities, late receipt handling, and
stale incoming/outgoing cancellation confirmations. Fixtures do not announce fake devices
on the LAN or overwrite user settings.

These checks support the implemented behavior; they do not prove complete parity.
Physical Android/iOS peers, real desktop portals/trays, fractional scaling and a
hands-on side-by-side workflow review remain necessary before claiming equivalence.
