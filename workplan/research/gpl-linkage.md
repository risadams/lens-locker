# Research: GPL-3.0 Linkage Implications of `jpegxl-rs`

Ticket: [../tickets/020-research-gpl-linkage.md](../tickets/020-research-gpl-linkage.md)
Status: findings complete, open questions flagged for decision (feeds
[021-decide-project-license](../tickets/021-decide-project-license.md)).

**Not legal advice.** Engineering-facing research from primary sources (the
jpegxl-rs repo, gnu.org's GPL FAQ/license text, named real-world precedents) to
inform a build decision. Everything in §3 needs real legal review before it's
release-blocking — and per §1.3, nothing here is urgent yet: LensLocker has
never been conveyed to anyone.

## 1. Summary — Decision-Oriented

1. **Static vs. dynamic linking does not matter for GPL-3.0, unlike LGPL.**
   FAQ: linking a GPL work "statically or dynamically with other modules is
   making a combined work... the terms and conditions of the GNU General
   Public License cover the whole combination." GPL has no equivalent of
   LGPL's "dynamic link + relinkable object code" escape hatch. Source:
   [GPL FAQ §GPLStaticVsDynamic](https://www.gnu.org/licenses/gpl-faq.html#GPLStaticVsDynamic).

2. **`jpegxl-rs` links dynamically by default** (`pkg-config`/`DEP_JXL_LIB`,
   panics if not found); **static linking is opt-in via the `vendored` feature**
   (`cmake`-builds libjxl from vendored source). Per point 1 this is a
   build-mechanics choice, not a licensing one — but `vendored`/static is the
   only realistic mode for LensLocker's single-static-binary goal, since end
   users' Windows machines don't have a system `libjxl.dll`. Source:
   [jpegxl-sys/build.rs](https://github.com/inflation/jpegxl-rs/blob/master/jpegxl-sys/build.rs),
   [jpegxl-rs/README.md](https://github.com/inflation/jpegxl-rs/blob/master/jpegxl-rs/README.md).

3. **The obligation only triggers at *conveying* (distributing), not at
   building/running privately.** GPLv3 §2: "You may make, run and propagate
   covered works that you do not convey, without conditions." LensLocker has
   never shipped a build to anyone — **the clock hasn't started.** Source:
   [GPLv3 §2](https://www.gnu.org/licenses/gpl-3.0.en.html#section2).

4. **Once conveyed, the whole binary must go out under GPL-3.0-compatible
   terms** — full copyleft (source availability to every recipient), not just
   attribution. FAQ: "the terms of the GPL apply to the entire combination...
   the work as a whole must be licensed under the GPL." Source:
   [GPL FAQ §IfLibraryIsGPL](https://www.gnu.org/licenses/gpl-faq.html#IfLibraryIsGPL).

5. **The GPL license is the wrapper author's choice, not inherited from
   libjxl.** libjxl itself (the C++ reference impl) is **BSD-3-Clause**, and
   even `jpegxl-src` (vendors/builds libjxl for the `vendored` feature) is
   BSD-3-Clause — only the hand-written `jpegxl-sys`/`jpegxl-rs` binding code
   is GPL-3.0-or-later. A non-GPL path to the same jbrd feature exists in
   principle: bind directly against libjxl's own BSD C API (§2.5).

6. **The `cjxl`/`djxl` subprocess approach has real FSF-primary-source support,
   and is a double mitigation.** "Mere aggregation": programs connected by
   "pipes, sockets and command-line arguments... normally" are **separate
   programs**, unless communication is "intimate enough, exchanging complex
   internal data structures." A subprocess call with file paths/flags fits the
   FSF's description of the normal, non-combined case — and since `cjxl`/
   `djxl` are BSD-licensed libjxl binaries, this removes the GPL crate
   entirely, not just an aggregation argument. **Caveat**: FSF itself calls the
   line "a legal question, which ultimately judges will decide" — real legal
   review is warranted before relying on this alone (§3).

7. **Precedent survey found three patterns**, no commercial one: adopt GPL for
   the component touching `jpegxl-rs` (`pillow-jpegxl-plugin`), stay
   permissive and gate it behind a non-default opt-in Cargo feature (`simp`,
   `img-optimize`, `aqiv`, `nef-compactor`), or go full GPL (`ferrite`). No
   closed-source precedent exists — flagged as a gap, not a validated path.

## 2. Detailed Findings

### 2.1 How `jpegxl-rs` links (Q1)

Repo ([github.com/inflation/jpegxl-rs](https://github.com/inflation/jpegxl-rs))
is a workspace of `jpegxl-sys` (raw FFI, `license = "GPL-3.0-or-later"`),
`jpegxl-rs` (safe wrapper, same license), `jpegxl-src` (vendors + `cmake`-builds
libjxl, `license = "BSD-3-Clause"`). The "-or-later" grant is spelled out in
`jpegxl-sys/build.rs`'s header: *"redistribute it and/or modify it under the
terms of the GNU General Public License... version 3... or (at your option) any
later version."*

Build logic, verified from `build.rs`: no `vendored` feature → `DEP_JXL_LIB` env
var, else `pkg_config::Config::new().probe("libjxl")`/`"libjxl_threads"`,
**panics** if not found (dynamic link against a system lib). `vendored` feature
→ `jpegxl_src::build()`, `cmake`-compiles and statically links. README: *"To
build `libjxl` and statically link it, use the `vendored` feature."* Source:
[jpegxl-rs/README.md](https://raw.githubusercontent.com/inflation/jpegxl-rs/master/jpegxl-rs/README.md).

Contrast with LGPL, straight from the same FAQ page: *"(1) If you statically
link against an LGPLed library, you must also provide your application in an
object... format, so a user has the opportunity to modify the library and
relink... (2) If you dynamically link against an LGPLed library already present
on the user's computer, you need not convey the library's source."* GPL-3.0
offers no such carve-out either way. Source:
[GPL FAQ §LGPLStaticVsDynamic](https://www.gnu.org/licenses/gpl-faq.html#LGPLStaticVsDynamic).

### 2.2 GPL-3.0/FAQ obligations on a combined work (Q2)

From **GPLv3 text** ([gnu.org/licenses/gpl-3.0.en.html](https://www.gnu.org/licenses/gpl-3.0.en.html)):

- §0: "convey" = propagation "that enables other parties to make or receive
  copies."
- §2: *"You may make, run and propagate covered works that you do not convey,
  without conditions."*
- §1, "Corresponding Source": includes source for "shared libraries and
  dynamically linked subprograms that the work is specifically designed to
  require, **such as by intimate data communication or control flow**" — the
  same "intimate communication" test the FAQ uses for plug-ins/subprocesses
  (§2.4) is written into the license text itself.
- §5(c): *"You must license the entire work, as a whole, under this License to
  anyone who comes into possession of a copy... no permission to license the
  work in any other way."*
- §5 Aggregate clause: independent works "not combined... to form a larger
  program" on the same medium is an "aggregate"; the License doesn't spread to
  the rest of an aggregate.

From the **FAQ**:

> **§IfLibraryIsGPL** — "If a library is released under the GPL (not the
> LGPL)... **Yes**, because the program actually links to the library... the
> work as a whole must be licensed under the GPL."

> **§LinkingWithGPL** — "you must release your program under a license
> compatible with the GPL... The combination itself is then available under
> those GPL versions."

**Answer to Q2**: yes — if LensLocker links `jpegxl-rs` (static or dynamic) and
is then *conveyed* to end users, the whole executable must go out under
GPL-3.0-or-later-compatible terms: full source availability to every
recipient, no additional restrictions. Full copyleft, not attribution-only.

### 2.3 Precedents (Q3)

| Project | Own license | How `jpegxl-rs` is used |
|---|---|---|
| [`pillow-jpegxl-plugin`](https://github.com/Isotr0py/pillow-jpegxl-plugin) — PyO3 native plugin adding JXL to Pillow | **GPL-3.0** | Statically links `jpegxl-rs` into a `cdylib`; plugin is separately installed/licensed, so Pillow (MIT) itself is unaffected |
| [`simp`](https://github.com/Kl4rry/simp) — real GPU-accelerated Rust desktop image viewer | **Apache-2.0** | `jpegxl-rs` is a **non-default** Cargo feature (`jxl`), only via opt-in `full`. README: *"Windows does not have any optional formats enabled by default"* — the typical shipped binary never combines with it |
| [`img-optimize`](https://github.com/mkaraki/img-optimize), [`aqiv`](https://github.com/arabianq/aqiv), [`nef-compactor`](https://github.com/pimlu/nef-compactor) | **MIT** each | Same non-default optional-feature pattern as `simp` |
| [`ferrite`](https://github.com/master-of-zen/ferrite) | **GPL-3.0-or-later** | Older version required `jpegxl-rs` unconditionally; project just went full GPL |
| [ImageMagick's Windows `jpeg-xl` build](https://github.com/ImageMagick/jpeg-xl) (C, for contrast — not Rust, doesn't use `jpegxl-rs`) | ImageMagick License (permissive) | Links libjxl **directly** via its C API; vendored LICENSE is the same BSD-3-Clause text as upstream |

Source for the crate list: [crates.io reverse-deps for jpegxl-rs](https://crates.io/api/v1/crates/jpegxl-rs/reverse_dependencies).
**No closed-source/commercial precedent found** — `jpegxl-rs`'s consumer base
is small hobby/FOSS projects (7 crates total), and none show visible evidence
of legal review behind their choice (see §3.6).

**Broader analog**: FFmpeg (LGPL-2.1-or-later core, optional GPL-2-or-later
parts) is the most mature real case of this exact split. Its own guidance is to
**avoid the GPL parts entirely**, not rely on subprocess isolation: *"FFmpeg is
licensed under... LGPL... However, FFmpeg incorporates several optional
parts... covered by the GPL... If those parts get used the GPL applies to all
of FFmpeg."* Source: [ffmpeg.org/legal.html](https://www.ffmpeg.org/legal.html).
Even the most experienced ecosystem here defaults to "don't take on the GPL
component," not "isolate and rely on aggregation."

### 2.4 What the CLI-subprocess approach changes (Q4)

> **§GPLPlugins** — "If the main program uses fork and exec to invoke
> plug-ins, and they establish intimate communication by sharing complex data
> structures... that can make them one single combined program. **A main
> program that uses simple fork and exec... and does not establish intimate
> communication... results in the plug-ins being a separate program.**"

> **§MereAggregation** — "pipes, sockets and command-line arguments are
> communication mechanisms normally used between two separate programs. So
> when they are used for communication, the modules normally are separate
> programs. But if the semantics of the communication are intimate enough,
> exchanging complex internal data structures, that too could be a basis to
> consider the two parts as combined." Containers don't change this analysis
> (§AggregateContainers). Source:
> [GPL FAQ §GPLPlugins](https://www.gnu.org/licenses/gpl-faq.html#GPLPlugins).

**Practical read**: invoking `cjxl`/`djxl` with file paths/flags on the command
line, reading an exit code and output file — no shared memory, no cross-boundary
function calls, no libjxl-internal data structures exchanged — is squarely the
FSF's *normal, non-combined* case. And since `cjxl`/`djxl` are themselves
BSD-3-Clause libjxl binaries, this path drops `jpegxl-rs` (the GPL crate) from
the build entirely — not just an aggregation argument, but no GPL code linked
at all.

**Caveat**: FSF itself says the line is "a legal question, which ultimately
judges will decide" — real-world-relied-upon opinion, not adjudicated case law
for this fact pattern. If LensLocker's wrapper ever grows beyond simple
file/argument passing, re-run this analysis. Practical costs (bundling
libjxl's pre-1.0 CLI binaries cross-platform, subprocess management) are
already covered in [recompression.md §2 / open question 1](recompression.md) —
not re-derived here.

### 2.5 A fourth option: bind directly against libjxl's BSD C API

Since libjxl is BSD-3-Clause and only the `jpegxl-sys`/`jpegxl-rs` *binding
code* is GPL by its author's choice, LensLocker could write new, minimal FFI
bindings (`bindgen` against libjxl's public headers) covering just
encode+jbrd-reconstruct, bypassing `jpegxl-sys`/`jpegxl-rs` entirely.
`jpegxl-src` (BSD-3-Clause) already solves "vendor + `cmake`-build libjxl" and
could be reused as-is. This keeps the in-process Rust API (the reason
`jpegxl-rs` was chosen over the CLI per [ticket 009](../tickets/009-conversion-policy.md))
with zero GPL cost, at the price of writing/maintaining unsafe bindings against
a pre-1.0, evolving C API instead of using a maintained crate. Not in the
original ticket, but a legitimate middle path between "accept GPL" and
"subprocess."

## 3. Open Questions Requiring Real Legal Review

1. Whether the "mere aggregation" subprocess argument (§2.4) actually holds for
   LensLocker's *specific* IPC implementation once designed — review before
   shipping on that basis, not after.
2. Whether publishing LensLocker's own source (a Cargo.toml merely *declaring*
   a `jpegxl-rs` dependency, no compiled binary ever conveyed) itself triggers
   any obligation on LensLocker's source license — unclear from primary
   sources surveyed here.
3. What exactly counts as LensLocker's first "conveying" event in practice (one
   beta build shared, a private update server, open-sourcing the repo
   pre-release) — don't assume informally that "no one has a build yet"
   protects every choice made right up to public release.
4. If §2.5's direct-BSD-binding option is pursued, confirm none of
   `jpegxl-sys`'s GPL-licensed binding code (struct layouts, constants) is
   reused — clean-room the header parsing.
5. If GPL-3.0-or-later is accepted for all/part of LensLocker, confirm the
   mechanics with counsel (full Corresponding Source to every recipient,
   preserved downstream) against any future paid tier, code signing, or EULA
   plans.
6. The precedent survey (§2.3) is not validation — none of the surveyed
   projects show evidence of legal review behind their choice; don't treat
   "other small projects did X" as legal cover for LensLocker.
