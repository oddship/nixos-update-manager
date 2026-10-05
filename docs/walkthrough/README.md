# Walkthrough captures

The README GIF contains twelve genuine GNOME Wayland screenshots from a disposable guest, held for 45 seconds with waiting omitted. It predates the NixOS Update Manager rename, grouped review, and current Settings flow. Historical filenames are retained.

The animation follows a synthetic local Git input through real NixOS Check/Prepare, background completion, review, graphical polkit authentication, exact Apply, and a lock-only commit. Candidates and successful outcomes were not seeded. The fixture has two hosts and an inert 90-second build delay to make window closure observable; the delay is test scaffolding, not app latency.

The guest uses synthetic account `tester` and password `disposable-vm-only`. No host home, configuration, or credentials are mounted. The authentication frame has an empty password field. These captures describe one pinned x86_64-linux GNOME guest, not a broad compatibility or interruption matrix.

## Later review and Settings captures

- [Grouped input review](../evidence/ui-grouped-packages.png): the final renamed package displaying retained synthetic review state for ten exported packages at 1.1.0. The [package list](../evidence/ui-grouped-package-list.png) shows the earlier native development capture, prepared without activation.
- [Settings](../evidence/ui-assistant-settings.png): the final packaged app, with explanations disabled until an explicit connection test. No model request was sent for this capture.
- [Contextual Explain](../evidence/ui-assistant-contextual.png): earlier native development UI with explicitly synthetic `fake_pi.py` responses, not a real provider. No provider login or cloud request was performed.
- [Dark review](../evidence/ui-dark-review.png) and [indicator while Ready](../evidence/ui-indicator-ready.png): earlier native review/indicator captures.

The grouped review and Settings screenshots were refreshed after the rename; the other captures retain the earlier title. The assistant connection test sends a short request only on a click, without update data. Saved status reflects the last test rather than live health. See [usage](../USAGE.md#update-assistant) for the current behavior.

## Animation frames

1. [First launch](frames/01-start.png)
2. [Native folder selection](frames/02-folder.png)
3. [Inferred hostname](frames/03-host.png)
4. [Check progress](frames/04-checking.png)
5. [Input updates found](frames/05-updates.png)
6. [Preparation progress](frames/06-preparing.png)
7. [Indicator with the app closed](frames/07-background.png)
8. [Completed preparation notification](frames/08-notification.png)
9. [Reopened runtime review](frames/09-review.png)
10. [Apply confirmation and optional commit](frames/10-confirmation.png)
11. [Graphical authentication](frames/11-authentication.png)
12. [Applied system and lock-only commit](frames/12-complete.png)

Regenerate with `bash scripts/walkthrough-gif` (ImageMagick and Fontconfig required). The encoder defaults to at most two threads. Relative media links let GitHub display the GIF without an external video service.
