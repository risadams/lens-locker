# Local font files

`ui/styles.css` declares `@font-face` rules for the LensLocker Brand System's
type system, pointing at files expected in this directory. Nothing in the
app fetches fonts over the network (zero network access, ever — CLAUDE.md),
so these files must be added locally. Until they exist, the CSS fails
closed to its system-font fallback chain (Segoe UI / Cascadia Mono) with no
visual break.

## Where to get them

- **Inter** and **Space Grotesk**: https://fonts.google.com/specimen/Inter
  and https://fonts.google.com/specimen/Space+Grotesk — click "Get font" /
  "Download all" to get a zip. Inside, look for a `static/` folder — it has
  individually-named per-weight `.ttf` files (no conversion needed).
- **JetBrains Mono**: https://github.com/JetBrains/JetBrainsMono/releases —
  download the latest release zip, the per-weight files are under
  `fonts/ttf/`.

Both are open source (SIL OFL 1.1) and fine to use in a personal, non-
distributed app per this project's license posture.

## Exact filenames expected

Copy/rename the matching weight from each source's `.ttf` into this folder
with these exact names:

| File | Family | Weight |
|---|---|---|
| `SpaceGrotesk-Medium.ttf` | Space Grotesk | 500 |
| `SpaceGrotesk-SemiBold.ttf` | Space Grotesk | 600 |
| `SpaceGrotesk-Bold.ttf` | Space Grotesk | 700 |
| `Inter-Regular.ttf` | Inter | 400 |
| `Inter-Medium.ttf` | Inter | 500 |
| `Inter-SemiBold.ttf` | Inter | 600 |
| `Inter-Bold.ttf` | Inter | 700 |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono | 400 |
| `JetBrainsMono-Medium.ttf` | JetBrains Mono | 500 |
| `JetBrainsMono-SemiBold.ttf` | JetBrains Mono | 600 |

Google's zip usually already names the `static/` files exactly this way
(`Inter-Medium.ttf`, `SpaceGrotesk-SemiBold.ttf`, etc.) — just copy them
over. JetBrains Mono's release zip may have slightly different casing
(`JetBrainsMono-Regular.ttf` is typical); rename if needed to match the
table above, since the `@font-face` rules reference these names literally.
