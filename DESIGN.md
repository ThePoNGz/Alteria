# Alteria — Design & Tech Stack Notes

> **Name:** **Alteria** — named after the **Alt** key, the modifier the entire quasimode
> model is built on. (Was codename `quasi`, after *quasimodes*.)
> **Status:** Brainstorming / pre-implementation — core stack decided 2026-05-28
> **Last updated:** 2026-05-28
> **Owner:** thepong — built collaboratively via Claude Code

A keyboard-centric, **non-modal** code editor in Rust. Your concept; this doc just
captures the decisions we've made and the open questions still to settle.

---

## 1. The concept (yours)

The defining idea is **quasimodes** (a real HCI term from Jef Raskin's *The Humane
Interface*): temporary states that exist **only while a modifier key is physically
held**, instead of persistent modes you toggle into (vim's Normal mode, Caps Lock).
Release the key → you're instantly back to plain typing. You can never get "stuck" in
the wrong mode.

Core mechanics (from your concept doc):

- **Typing is the default state.** No Insert mode. Keys type text unless a modifier is held.
- **Hold `Alt` + WASD / IJKL** = inverted-T navigation (ergonomic alternative to vim's hjkl).
- **Anchor/Head selection model** — every cursor is `{ anchor, head }`; `anchor == head`
  means no selection, otherwise the span between them is selected.
- **Multicursor** as a `Vec<Cursor>`; edits apply to all cursors at once.
- **Context-aware expansion** (`Alt+I`) — progressively select word → string → brackets.
- **Inline char search** (`Alt+F`) — jump to a character on the current line.

The full keymap lives in your original concept PDF; this doc doesn't re-transcribe it.

---

## 2. The biggest decision: it's a **GUI app, not a terminal app**

The concept started as a *terminal* editor, but the held-modifier design is fundamentally
incompatible with terminals:

- Classic terminals send a one-way byte stream with **no key-up/key-down events** and
  **lossy modifier combos** (`Ctrl+S` is XOFF/freeze; `Ctrl`+letter collapses to control
  codes). The "while a key is held" model literally doesn't exist there.
- The only fix is the **Kitty Keyboard Protocol**, which works on modern emulators
  (kitty/foot/ghostty/wezterm) but **not in a bare TTY** — killing the "install it on a
  fresh Arch console" universality that's the whole point of a terminal editor.
- Insight: **vim's controls — the thing you dislike — exist *because* of that terminal
  limitation.** Wanting non-modal held-modifier controls *and* bare-TTY support is a
  contradiction.

A native GUI window gets **real OS key-down/key-up events, every modifier, no XOFF, no WM
stealing keys** — so the quasimode concept works *perfectly* and natively. The terminal was
only ever a proxy for "lightweight + keyboard-driven"; a GUI gives us those too (see Zed).

---

## 3. Goals & non-goals

**Goals**
- A **daily driver for you first**, built well enough that others *could* adopt it.
- **Genuinely fast** — but the bar is *"feels instant for the files I actually open,"*
  **not** matching Zed's 120fps-huge-file engineering. (That bar would make us feel slow forever.)
- **Free & open source.** Public GitHub repo eventually; open to contributors + Ko-fi.

**Non-goals (for now)**
- Not chasing Zed-tier rendering performance or collaboration features.
- Not a terminal/TTY editor (micro/nano/vim already own the bootstrap-console niche).
- Not building for hypothetical contributors — build it for you; keep it modular so the
  door stays open at zero cost.

---

## 4. Architecture principles

- **Decoupled core.** The editor *engine* (rope buffer, `Vec<Cursor>`, selections, edit
  transactions, **keymap→action** mapping) is a pure Rust library that knows **nothing**
  about rendering. The GPUI frontend translates raw input → abstract actions and renders
  engine state. Precedent: Helix's `helix-core`, Lapce's `floem-editor-core`.
  - *Why it matters here:* GPUI churns. If the core is frontend-agnostic, GPUI changing (or
    us swapping it later) never touches the engine.
  - *Confirmed (2026):* GPUI ships 2–3 **breaking** builds/week and just re-architected its
    whole platform layer. So this isn't a nice-to-have — it's our **primary churn mitigation
    and a hard rule**: GPUI types must never leak into `alteria-core`. Bonus — it keeps
    **Floem** (Lapce's framework, the strongest alternative) a low-cost fallback if GPUI's
    coupling/churn ever stops being worth it.
- **Controls-first.** The riskiest, most novel part is *whether the quasimode keymap feels
  good in the hand* — nobody knows yet. We validate that on the smallest possible frontend
  before investing in polish. (See Roadmap.)

---

## 5. Potential tech stack

| Layer | Choice | License | Notes |
|---|---|---|---|
| Language | **Rust** | — | |
| UI framework | **GPUI** (Zed's framework) | Apache-2.0 | Confirmed viable (2026). Access via a **pinned git rev of `zed-industries/zed`** — the published `gpui` crate (0.2.2) is stale and `gpui-ce` lags ~2mo. Pre-1.0: 2–3 **breaking** builds/week → pin deliberately, bump on purpose. |
| GPU rendering | **wgpu** (*comes via GPUI*) | MIT/Apache | Gives "install it, works on any GPU" — auto-picks Vulkan/Metal/DX12/WebGPU. **Confirmed: GPUI migrated Blade→wgpu, shipped ~Zed 0.12 (Mar 2026)** — this fixed the old NVIDIA/Wayland Blade crashes. Needs a Vulkan-capable GPU. |
| Windowing/input | GPUI platform layer | Apache-2.0 | **Confirmed: `ModifiersChangedEvent` fires on Wayland *and* X11, independent of other keys** — the exact primitive quasimodes need. Bare modifiers route through `ModifiersChanged` (not KeyDown/KeyUp), so build held-Alt detection on that. |
| Text shaping/raster | **handled by GPUI** (bundles a cosmic-text stack) | — | Not a separate choice while on GPUI. Only relevant if we ever leave GPUI (then: cosmic-text + glyphon on raw wgpu). |
| Text buffer | **ropey 1.6.1** | MIT | The rope. Same crate Helix uses. Standalone — not from Zed. Feature-frozen/maintenance-mode *by design* (stable). 2.0-beta switches to byte-indexing — don't adopt the beta yet. |
| Coordinate model | store **byte offsets**; move cursor by **grapheme clusters**; derive visual columns | — | Matches tree-sitter's native byte/`Point` model + Helix practice. *(Resolves a former open question.)* |
| Undo/redo | edit **transactions/changesets**; multicursor edit = one **atomic** transaction; **revision-*tree*** history (à la Helix), not a linear stack; **anchors** that survive edits | — | *(Resolves a former open question.)* |
| Syntax highlighting | **tree-sitter** *(later / v2)* | MIT | Incremental parsing. Confirmed still the standard (`tree-sitter` 0.26.x, very active). Watch `arborium` for grammar-ABI distribution pain. Defer past v1. |
| Config | TBD *(likely a keymap config file)* | — | The keymap *is* the product, so it should be configurable. Later. |

> ⚠️ **Dropped from the original PDF:** `crossterm` + `ratatui`. Those were for the
> terminal approach we abandoned. Not used.

> ⚠️ **GPUI licensing nuance (resolved for us):** GPUI's *own* code is Apache-2.0, but it
> transitively links a GPL-3.0 crate (`gpui → sum_tree → ztracing`) as a non-optional dep —
> open issue **zed#55470** (May 2026, unresolved). This only bites projects shipping under a
> *non-GPL* license. **Since Alteria ships GPL-3.0 (§7), it's a non-issue:** a GPL binary
> linking GPL deps is fully compliant. (Not legal advice.)

---

## 6. How we use **Zed's framework as a reference** ← (the part you asked me to spell out)

GPUI is **niche, sparsely documented, and changes fast.** That has two consequences:

### a) Working loop — I read their repo *before* writing GPUI code
My (Claude's) training knowledge of GPUI is thin and likely **stale**, so writing GPUI from
memory = hallucinated/old APIs that won't compile. So our loop is:

1. Before building any GPUI feature, I pull the **current** source/examples:
   - the [GPUI book](https://matinaniss.github.io/gpui-book/),
   - the `gpui` crate docs,
   - examples in the Zed repo,
   - **Context7 MCP** when it has GPUI docs.
2. *Then* I write our code against the real, current API.

This is slower per step and it's the right kind of slow — real code over confident garbage.

### b) Legal / clean-room discipline — two kinds of code in Zed's repo
| What we're looking at | License | What we may do |
|---|---|---|
| **GPUI usage / patterns** | Apache-2.0 | Copy & adapt freely — that's the point of GPUI. |
| **Zed's *editor* logic** | GPL-3.0 | Read **only to understand the idea**, then write our **own** implementation. **Never paste.** |

**Concrete crate map (verified against the live repo, May 2026) — which side of the line each crate sits on:**

- **Apache-2.0 — may depend on / vendor / adapt freely:** `gpui` (+ `gpui_macros`,
  `gpui_platform`, `gpui_macos`, `gpui_linux`, `gpui_windows`, `gpui_wgpu`, `gpui_web`),
  `sum_tree`, `collections`.
- **GPL-3.0 — read for ideas ONLY, reimplement in our own words, never paste:** `rope`,
  `text`, `editor`, `language`, `multi_buffer`, `clock`, `fuzzy`.
- The license line runs *right between* `sum_tree` (Apache — the generic B-tree engine) and
  `rope` (GPL — the text buffer built on it). We use **ropey** for v1 regardless; the
  "feels-instant on my files" bar (§3) doesn't need SumTree-tier engineering.

> Reminder on the "as fast as Zed" goal: GPUI gives us Zed's **rendering** speed *by using it*
> (Apache — legitimate). The **engine** speed we earn ourselves by reimplementing ideas with
> our own crates — we never copy the GPL editor logic.

Ideas, algorithms, and architecture aren't copyrightable — *expression* (the literal code)
is. Reading how Zed solved a problem and reimplementing it in our own words carries **zero**
licensing obligation and keeps our license a free choice. Standalone crates (`ropey`,
`wgpu`, `tree-sitter`) come from their own upstreams, **not** lifted out of Zed.

> Flag me if you ever see me drifting toward copy-pasting Zed's editor source.

---

## 7. Licensing (ours)

**Decided (2026-05-28): GPL-3.0.** Strong copyleft — Alteria stays free forever, nobody can
fork it closed. It also cleanly clears the GPUI transitive-GPL concern (§5) and aligns with
Zed's own license. Full GPL-3.0 text now lives in `LICENSE`. (Not legal advice.)

Reasoning, kept for the record (we are **not** copying Zed's GPL code, so the license was a
free choice):

- **Chose copyleft (GPL-3.0 over MPL-2.0)** — guarantees the editor *stays* free and
  nobody can fork it closed-source. Matches the "free for everyone" intent. (Zed = GPL-3.0;
  Helix = MPL-2.0.) GPL-3.0 over MPL because, while we're on GPUI, GPL is the airtight choice
  for the transitive-GPL link noted in §5.
- MIT/Apache was the alternative if a closed fork didn't matter — declined.
- **Donations (Ko-fi/Sponsors) are compatible with *every* OSS license**, including
  copyleft. "Free software" = *freedom*, not *price*.
- Contribution setup is trivial: a `LICENSE` file is enough (inbound = outbound by GitHub
  convention). No CLA needed. Add a `CONTRIBUTING.md` later if people show up.

---

## 8. Roadmap (milestone-driven, not date-driven)

**M0 — Controls validation (the make-or-break milestone):**
Smallest possible GPUI window that:
- renders a `ropey` buffer,
- runs the **hold-Alt + WASD** quasimode keymap (move + basic selection).

No syntax highlighting, no config, no tabs, no file management. The single question to
answer: ***does navigating/selecting with this keymap actually feel good in my hand?***
Everything else is built on top *after* this feels right.

**Later milestones (rough, post-M0):** multicursor → selection expansion + inline search →
file open/save → multiple files → syntax highlighting (tree-sitter) → config/keymap file.

---

## 9. Open questions / next steps

- [x] **Name the project** → **Alteria** (after the **Alt** key — the modifier the whole
      quasimode model rides on). *Decided 2026-05-28.*
- [ ] **Define v1 scope:** is "edit one file with my keymap" enough to call it usable, or do
      you need tabs / search / save-as before you'd open it instead of micro? *(This sets
      the whole v1 boundary — still unanswered. **Next design frontier.**)*
- [x] **Text coordinate model** → store **byte offsets**, move by **grapheme clusters**,
      derive visual columns (see §5). *Decided 2026-05-28.*
- [x] **Undo/redo model** → **transaction/changeset + revision-tree history**, multicursor-
      aware, **anchor**-based (see §5). *Decided 2026-05-28.*
- [ ] Design the **input → intent → action pipeline** (where the quasimode logic lives —
      this is the heart of the concept). *Next design frontier — lives in `alteria-core`,
      fed plain-data events, headless-testable.*
- [x] `git init` + license → **done.** Repo linked to GitHub; **GPL-3.0 `LICENSE` added.**
- [ ] Turn this into a full implementation plan (writing-plans) — *after v1 scope + the input
      pipeline are designed.*

---

*This is a living notes doc, not a contract. Decisions here can change as we learn —
especially after M0 tells us whether the controls actually feel good.*
