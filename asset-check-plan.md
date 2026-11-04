# Asset Check - Plan

**Status: design, November 2026.** No code has been written. This document lives in the fire-decode
repo for now and moves to the rules repo (AC5) once that repo exists.

## The problem

More and more of the art arriving in the Unity project comes from AI-powered tools. Much of it is
unoptimised, or not built to the team's rules: wrong formats, odd sizes, unused alpha channels,
embedded prompts, and names that Unity's import presets don't recognise.

The goal is to catch this before it lands in the repository and tell the artist three things, in plain
language:
- **what** is wrong;
- **why** it matters;
- **how** to fix it.

Applying fixes automatically comes later (AC2).

The pieces:

| Piece | Role |
| --- | --- |
| **`fire-cli`** (new, ships with Fire) | Checks images, on fire-decode |
| **`3d-review-cli`** (3D Review) | Checks models; it already has 43 checks |
| **`asset-rules`** (new shared crate) | Decides which rules apply to which file, and defines the results format both CLIs write |
| **The rules repo** (a git submodule, written only by the technical artist) | Holds the rules, the hook scripts and the Launcher tools |
| **Launcher** | Shows the results to the artist |
| **The git `pre-commit` hook and a Fork custom command** | Run the checks |
| **A Unity editor tool** (after the pilot) | Runs the same checks when assets are imported |

Decisions are numbered **AC1-AC19**. Section 5 is the rollout with its gates. Appendix A lists the facts
about today's tools that the decisions rest on.

---

## 1. What the artist sees

1. The artist stages files in Fork and commits.
2. If the staged files break no rules, or only raise info-level notes, **nothing appears**. The commit
   goes through.
3. If there are warnings or errors, **the commit waits** and the Asset Check window opens:

```
┌ Asset Check: 2 files need attention before you commit ───────────────────┐
│                                                                          │
│ T_Crate_D.png       Checked as: Prop texture · Production                │
│                                         [Show in Explorer] [Open in Fire]│
│  ✖ Too large: 4096×4096. Prop textures may be at most 2048×2048.         │
│      Why: on Quest a texture this size takes 16 MB of memory…            │
│      How: scale the image down to 2048×2048 in Photoshop                 │
│           (Image > Image Size) and save it again.                        │
│      Note from Tech Art: the Quest budget for props is 2048.             │
│  ⚠ Has an alpha channel, but every pixel is fully opaque.                │
│      Why: …   How: …                                                     │
│                                                                          │
│ ComfyUI_00042_.webp  Checked as: (no texture type matches this name)     │
│                                         [Show in Explorer] [Open in Fire]│
│  ✖ Unity can't import WebP.   How: save it as PNG.                       │
│  ✖ The name doesn't match any texture type.                              │
│      How: rename it to T_<Name>_<Type>, e.g. T_Crate_D.png.              │
│                                                                          │
│             [Don't commit - I'll fix these]        [Commit anyway]       │
└──────────────────────────────────────────────────────────────────────────┘
```

4. **Don't commit** cancels the commit. The artist fixes the files, stages them again and commits again.
   **Commit anyway** lets it through. In enforce mode (AC2), "Commit anyway" is not offered while there are
   errors.
5. An artist can also check before committing. The Fork command **Check staged files** opens the same
   window, with **Check again** and **Close** instead of the commit buttons.

---

## 2. Decision record

### AC1 - Shape: the CLIs check, the Launcher shows, git triggers, nothing runs in the background

- **Checks** run in two command-line tools, one per kind of asset:
  - `fire-cli` for images (AC12);
  - `3d-review-cli` for models (AC13).
- **Results** are shown in a Launcher tool window (AC14).
- **Triggers** are the `pre-commit` hook and a Fork custom command (AC3).
- **No background process or file watcher** runs on artists' machines.

Considered:
- **Image checks inside Fire's viewer** - rejected:
  - Fire's release build on Windows has no console;
  - it takes nothing but a file path;
  - its README calls it "strictly an image viewer".
- **Image checks inside fire-decode** - rejected: the library supplies facts about an image; the team's
  rules are policy and belong in the tool that uses it.
- **A watcher process** that checks files the moment they are staged - rejected by choice (AC3).

### AC2 - Rollout modes: report first, block later, fix last

- **v1 is report-only and never blocks (advise mode).** Warnings and errors are shown; the artist decides.
- **Enforce mode**, switched on once the team is used to the checks:
  - errors block the commit, so "Commit anyway" is not offered while there are errors;
  - warnings still allow it.
- **The mode** is one field in `rules.json` (AC6). The technical artist switches it for everyone by
  updating the rules submodule.
- **Automatic fixes come last**, after the tools are shown to apply them consistently. The results format
  reserves an empty `fix` field for them (AC10). Until then, each finding's "how" text describes a manual
  fix in the artist's own tools (Photoshop, Maya, ...).

### AC3 - Triggers: the pre-commit hook, plus a manual Fork command

**Git has no hook that runs on staging.**
- Fork's custom commands start only by hand: from a context menu, the toolbar's "Open in" dropdown, or
  Quick Launch (Ctrl+P). Nothing shows an event trigger (Appendix A).
- Git's clean filters do run on `git add`, but Git LFS already owns them for binary assets, and they must
  not have side effects.

So:
- **Automatic: the `pre-commit` hook.** Fork runs it on every commit.
- **Manual: a Fork custom command, "Check staged files".**
  - Artists can run it any time before committing.
  - It opens the same window without the commit buttons.
  - Its results are cached (AC15), so the hook is then instant for the same files.
  - It is shared with the team through the repository (the exact mechanism is confirmed in spike S2).

Bypassing the hook is still possible (`--no-verify`, or a client that skips hooks). The GitHub check
(AC18) is the backstop that catches anything that slipped through.

### AC4 - The hook waits for the artist; the window appears only for warnings and errors

**The hook does this, in order:**
1. Runs both CLIs on the staged files and waits for them.
2. If no warning or error was found, it exits 0 with nothing shown. Info-level findings never open the
   window.
3. Otherwise it opens the Asset Check window and **waits for the artist's choice**:
   - **Don't commit** → the hook exits 1;
   - **Commit anyway** → the hook exits 0.

**Why the checks run before the window opens, not inside it:** a clean commit must show nothing.
Opening the window on every commit just to run the checks would flash it each time. The checks are
kept fast (AC15), and the Fork command gives artists a way to check early.

**This needs one Launcher change:** the window has to be able to end with an exit code chosen by a step.
Today it always exits 0 (AC14).

### AC5 - The rules repo

- **A git repository added to the Unity project as a submodule**, outside `Assets/` so Unity never
  imports it.
- **Only the technical artist can write to it;** everyone can read it.
- **A rules change reaches the team** when the Unity project moves its submodule pointer, so the
  technical artist controls when changes roll out.

Contents:

| Path | What |
| --- | --- |
| `rules.json` | Mode, stages, branch stages, file types, profiles, minimum tool versions (AC6) |
| `textures/*.json`, `models/*.json` | One rules file per profile per stage (AC8) |
| `hooks/pre-commit` | The real hook script (`sh`, portable to macOS) |
| `hooks/check-staged` | What the Fork command runs |
| `launcher/asset-check.launcher` | The results window (AC14) |
| `launcher/setup.launcher` | One-time setup on an artist's machine (AC16) |
| `fork/` | The shared Fork custom command definitions (if spike S2 shows they must live elsewhere, they're installed from here) |
| `docs/` | This plan, once it moves |

### AC6 - Rules are chosen by file type, then profile, then stage

For each staged file:
1. **The file type** comes from its extension, e.g. `texture`, `model`. A file whose extension belongs to
   no type is ignored.
2. **The profile** is the first profile of that type, in `rules.json` order, whose **name patterns**
   match. A profile can also give **path patterns**, which narrow the match further; paths are
   optional. Examples:
   - `SK_*`, `CH_*` → character model
   - `SM_*` → environment prop
   - `T_*_N` → normal map
3. **No profile matches?** That is a finding in its own right: "The name doesn't match any \<type\>
   type". It is a warning, or an error in enforce mode, and its "how" lists the expected name patterns.
   This matters because Unity's import presets choose compression by name: a badly named asset gets the
   wrong import settings without anyone noticing.
4. **The stage** is resolved as in AC7.
5. **The rules file** is the profile's file for that stage (AC8).

Profile names and pattern matching are case-sensitive, as in 3d-review-cli's naming rules.

### AC7 - Production stages, and which one wins

- **The stages are named in `rules.json`**, e.g. `prototype` and `production`.
- **A file's stage comes from the most specific source that sets one:**
  1. **The asset itself** - **reserved, not in v1.** The marking mechanism is decided later. Candidates:
     a Unity asset label (stored in the `.meta` file), a name tag, or a list file in the Unity repo.
  2. **The branch** - patterns such as `proto/*` → `prototype` and `main` → `production`. This is the
     branch being committed to; a detached HEAD matches no branch rule.
  3. **The profile** - a profile can set its own stage, e.g. characters already in production while
     props are still prototype.
  4. **The project** - the default stage in `rules.json`.
- **The window shows the stage that applied** ("Checked as: Prop texture · Production"), so a surprising
  result is easy to explain.

### AC8 - One complete rules file per profile per stage

- **Each profile maps every stage to a rules file**, with `"*"` as an optional catch-all, e.g.
  `{"prototype": "models/character-proto.json", "production": "models/character.json"}`.
- **Files are complete, not overlays**, so the rules that apply are always written out somewhere.
  - Model rules files use 3d-review-cli's existing profile format, so they can be edited and saved in
    3D Review's profile editor.
  - Texture rules files use the same envelope (AC12).
- **Every rule can carry a `note`** from the technical artist, shown under the tool's own why text, e.g.
  "Quest budget for props".
- **`asset-rules` checks the configuration** for gaps (a stage with no file, a missing file, a pattern
  that can never match because an earlier profile always wins) through a `rules check` command (AC9).

### AC9 - `asset-rules`: one shared crate decides the rules for both CLIs

The hook, the Fork command, the Unity tool and CI must always give identical answers. So the selection
logic lives in **one small Rust crate**, used by both `fire-cli` and `3d-review-cli`. It is kept in its own
repository and used by git tag, like fire-decode.

**It owns:**
- the `rules.json` format (AC6-AC8);
- type, profile, stage and rules-file resolution;
- the unmatched-name finding;
- the shared results format and its writer (AC10);
- the git helpers: listing staged files, reading the current branch, and reading staged content
  (AC15);
- the cache (AC15);
- the minimum-tool-version check.

**Commands each CLI gets through it:**
- `rules check <rules.json>` - validates the configuration.
- `rules explain <file>` - shows which type, profile, stage and rules file apply to a file, and why. This
  is for the technical artist.

**It does not know what a texture or model rule means.** It hands each CLI the path of its rules file,
and the CLI parses that file and runs the checks.

Considered:
- **A separate front-end tool** that resolves rules and calls the CLIs - rejected: one more program to
  build and ship to every artist.
- **Each CLI implementing `rules.json` itself** - rejected: two copies would drift apart.

### AC10 - One results format, written by both CLIs

`--results <file>` writes `asset-check.results` version 1. Both CLIs write the same shape, so the
Launcher window, the Unity tool and CI read one format. Sketched in section 3.

**Per file:**
- path, type, profile, stage;
- status: checked, unreadable, or skipped;
- a problem message when it couldn't be checked;
- its findings.

**Per finding:**
- rule id and severity;
- a short title;
- **what**, with the numbers, e.g. "4096×4096; prop textures may be at most 2048×2048";
- **why** and **how** - default text built into the tool;
- the technical artist's **note**;
- **measured** and **limit**;
- **objects** - the affected objects inside a model;
- **fix** - always `null` until AC2's fix phase.

**Exit codes follow 3d-review-cli's existing convention:**
- 0 - nothing at warning level or worse;
- 1 - something was found, or a file couldn't be checked;
- 2 - the command line couldn't be followed.

### AC11 - Messages

- **What** gives the measured value and the limit, never just the rule's name.
- **Why** gives the consequence in the game or the pipeline ("takes 16 MB of memory on Quest"), not the
  rule's own reasoning.
- **How** names the steps in the artist's tool ("Photoshop: Image > Image Size").
- **Plain language, short sentences, no internal jargon.**
- English only for now. Both tools keep their text in a message catalogue, as 3D Review already does,
  so translation can come later.

### AC12 - `fire-cli`: a new console tool for images, built on fire-decode

**Where it lives:**
- A new crate and binary in fire's repo, installed with Fire, beside `fire.exe`. This mirrors
  `3d-review-cli` beside `3d-review.exe`.
- It doesn't wait for the fire-decode extraction (`fire-decode-plan.md`); it uses fire-decode as a
  workspace crate today.

**Commands:**
- `check <files>... | --staged | --files-from <list>`, with `--rules <rules.json>` and
  `--results <file>`;
- `info <file>` - prints what fire-decode sees;
- the `rules` commands from AC9.
- **`--files-from` exists because** Windows limits a command line to about 32,000 characters, which
  a big commit could exceed.

**Decoding:** files are decoded in parallel at full size, with no downscale and no ICC conversion, so the
alpha scan and the colour-profile test see the real image. Full decodes are fast enough for v1
(typically 50-400 ms per texture); fire-decode's planned `probe()` can speed this up later.

**Rules for v1:**

| Rule | Measures | Needs |
| --- | --- | --- |
| `texture.format` | Format against an allowed list (e.g. PNG, TGA, PSD, EXR, TIFF). Flags formats such as WebP, AVIF and HEIC, which Unity's texture importer doesn't list as supported (confirm for the Unity version in use) | fire-decode (today) |
| `texture.max_size` / `texture.min_size` | Width and height against limits | fire-decode (today) |
| `texture.power_of_two` / `texture.multiple_of_4` / `texture.square` | Dimensions | fire-decode (today) |
| `texture.unused_alpha` | Alpha channel present, every pixel opaque | fire-decode (today) |
| `texture.alpha_required` | No alpha where the profile needs one (masks, cut-outs) | fire-decode (today) |
| `texture.bit_depth` | Source bit depth against allowed depths (16-bit only where allowed, e.g. height maps) | fire-decode, plus a fix: palette PNGs report 8 today |
| `texture.color_profile` | Embedded ICC profile that isn't sRGB (Unity ignores ICC, so colours shift) | fire-decode: expose its existing private sRGB test |
| `texture.generation_metadata` | AI prompts or workflows embedded in PNG text chunks or XMP. Policy: strip them | Read by fire-cli itself |
| `texture.file_size` | File size against a limit | - |
| `texture.psd_composite` / `texture.psb` | PSD saved without a merged image (Unity needs it: "save with Maximize Compatibility"); PSB not allowed | fire-decode: a distinct error for "no merged image" |

Later, info-level rules: greyscale content saved as RGB, and normal-map sanity checks for files whose
profile is a normal map.

**Rules file format:** 3d-review-cli's envelope:
- `{version, generator, profile: {name, rules: {<id>: {enabled, severity, params, note}}}}`;
- loaded leniently, as 3d-review-cli does: unknown rules are skipped, and a missing rule takes the tool's
  default.

### AC13 - `3d-review-cli` changes

- **`--rules <rules.json>`** through `asset-rules`. It picks the model profile and stage per file and
  loads that file's profile. `--profile` keeps working for direct use.
- **`--results <file>` in the shared format.**
  - It includes each rule's **what / why / how** text, which 3D Review already has for all 43 checks but
    today shows only in the viewer's Inspector.
  - It includes the offending object names, which are already in its report.
- **`--staged` and `--files-from`.**
- **`note` on every rule** in its profile format.
- **Input:** FBX only, as today. Other model formats are out of scope.

### AC14 - Launcher changes

All three are generic, so other tools can use them too:
- **A results table control.**
  - Rows are read from a JSON file whose path is a value in the form.
  - Rows can be grouped by a field (here: one group per file, one row per finding), with a severity icon,
    a title line, and expandable detail text.
  - Each row can have buttons bound to steps; a step sees the clicked row's fields as `{row.field}`.
  - The table refreshes after a step finishes, and it scrolls.
- **Pre-filling the window from the command line:** `launcher FILE --set ID=VALUE`. Today only an
  environment variable can do this.
- **Ending the window with an exit code chosen by a step**, so the hook can read the artist's choice
  (AC4).

`asset-check.launcher` (in the rules repo):
- Reads the results path and the launch context, either `hook` or `manual`.
- Per file: **Show in Explorer** / **Show in Finder** (the existing `files.show-in-folder`) and **Open**
  (in Fire or 3D Review, by type).
- At the bottom, depending on the context:
  - from the hook: **Don't commit - I'll fix these** (the default button) and **Commit anyway** (left out
    in enforce mode when there are errors);
  - from the Fork command: **Check again** and **Close**.

### AC15 - What gets checked, and the cache

- **The staged version of each file**, not whatever happens to be on disk.
  - If the working file matches what is staged (the usual case for art), it is checked in place.
  - If it has unstaged changes, the staged content is checked instead: for a Git LFS file, from its
    object in `.git/lfs/objects/`; otherwise extracted with `git cat-file` to a temporary file.
- **Only files added or changed in the commit** are checked, so existing content never raises
  findings.
- **The cache** lives in `.git/asset-check/cache/`. Each entry is keyed on four things:
  - the staged blob id (git's own content hash; for an LFS file, the pointer's hash, which includes the
    content's hash);
  - the digest of the rules that applied;
  - the tool's version;
  - the resolved stage.

  The Fork command fills the cache, so the hook at commit time is instant for files already checked.
- **Target:** a typical commit with up to 20 assets is checked in a few seconds without the cache.

### AC16 - Setup on an artist's machine, and missing pieces

**`setup.launcher`, run once:**
- checks that Fire, 3D Review and the Launcher are installed, at the minimum versions `rules.json` asks
  for;
- records the tools' paths in `.git/asset-check/tools.env`, so the hook doesn't search on every
  commit;
- installs `.git/hooks/pre-commit`.

**The installed hook is a three-line delegator** that calls `hooks/pre-commit` inside the rules
submodule, chaining any hook already there.
- So changes to the hook roll out with the submodule.
- It deliberately avoids `core.hooksPath`, which would switch off Git LFS's own hooks.

**When something is missing, the window says so as a single finding**, with the fix:
- tools not installed or too old ("Update 3D Review to 0.9");
- the rules submodule not checked out ("In Fork: Submodules > Update").

**If the Launcher itself is missing,** the hook prints a message and lets the commit through. This is why
setup must succeed first.

**Planned with the Unity tool (AC17):** the Unity project checks for the hook when it opens and offers
to install it, so most artists never run setup by hand.

### AC17 - The Unity editor tool (after the pilot, planned now)

- **On import:** an `AssetPostprocessor` collects imported assets of known types and runs the CLIs in
  the background with `--rules` and `--files-from`. It shows the results in an editor window with the
  same text, plus **Select** and **Ping** buttons.
- **It checks only new local work:** assets git reports as untracked or modified. Re-imports caused by a
  pull or a branch switch are skipped, so a large pull doesn't flood the window.
- **Import presets are unchanged.** Unity's presets still choose compression by name, which is why naming
  is a rule (AC6).
- **It reads the same rules submodule** and finds the tools through the same `tools.env` (AC16).
- **Hook installation:** it checks for the hook when the project opens, as described in AC16.

### AC18 - The GitHub check (later)

- **A GitHub Actions job on a Windows runner** runs both CLIs on a pull request's changed files, with
  the same rules submodule. The job needs read access to the rules repo.
- **It reports first:**
  - a job summary listing each file's findings;
  - a passing status while the project is in advise mode.
- **Later it becomes a required check** when enforce mode starts.
- **The branch stage (AC7) uses the pull request's target branch**, because that is where the assets will
  live.

### AC19 - Platforms

- **Windows first, since the team is on Windows.** Everything is kept working on macOS (Apple silicon):
  - Fire, 3D Review and the Launcher already ship there;
  - the hook scripts are `sh`;
  - the window uses Show in Finder.
- **macOS caveat:** git clients on macOS run hooks with a minimal PATH, so the hook finds its tools only
  through `tools.env`.

---

## 3. Formats (sketches)

Illustrative only. Names settle when `asset-rules` is built.

### `rules.json`

```json
{
  "version": 1,
  "mode": "advise",
  "stages": ["prototype", "production"],
  "stage": "prototype",
  "branches": [
    { "pattern": "proto/*", "stage": "prototype" },
    { "pattern": "main",    "stage": "production" }
  ],
  "requires": { "fire-cli": "0.1.0", "3d-review-cli": "0.9.0" },
  "types": {
    "texture": { "tool": "fire-cli",   "extensions": ["png", "tga", "psd", "psb", "tif", "tiff", "exr",
                                                     "jpg", "jpeg", "webp", "avif", "heic", "heif"] },
    "model":   { "tool": "3d-review-cli", "extensions": ["fbx"] }
  },
  "profiles": [
    { "id": "character-model", "type": "model", "label": "Character model",
      "names": ["SK_*", "CH_*"], "stage": "production",
      "rules": { "*": "models/character.json", "prototype": "models/character-proto.json" } },
    { "id": "prop-model", "type": "model", "label": "Environment prop",
      "names": ["SM_*"], "paths": ["Assets/Art/Environment/**"],
      "rules": { "prototype": "models/prop-proto.json", "production": "models/prop.json" } },
    { "id": "normal-map", "type": "texture", "label": "Normal map",
      "names": ["T_*_N"],
      "rules": { "*": "textures/normal.json" } }
  ]
}
```

- **Pattern scope:** `names` match the file name without its extension; `paths` match the
  repository-relative path.
- **The `prototype` entry** in the character profile only takes effect on a `proto/*` branch, because the
  branch outranks the profile's own stage (AC7).

### A texture rules file

```json
{
  "version": 1,
  "generator": "hand-written",
  "profile": {
    "name": "Prop texture (production)",
    "rules": {
      "texture.max_size":     { "severity": "error",   "params": { "max": 2048 },
                                "note": "Quest budget for props." },
      "texture.power_of_two": { "severity": "error" },
      "texture.unused_alpha": { "severity": "warning" },
      "texture.format":       { "severity": "error",   "params": { "allowed": ["png", "tga", "psd"] } },
      "texture.generation_metadata": { "severity": "warning" }
    }
  }
}
```

### Results

```json
{
  "schema": "asset-check.results",
  "version": 1,
  "tool": { "name": "fire-cli", "version": "0.1.0" },
  "rules": { "commit": "1a2b3c4", "digest": "sha256:…" },
  "context": { "branch": "main", "mode": "advise" },
  "files": [
    {
      "path": "Assets/Art/Props/T_Crate_D.png",
      "type": "texture", "profile": "prop-texture", "profile_label": "Prop texture",
      "stage": "production", "stage_from": "branch",
      "status": "checked", "problem": null,
      "findings": [
        {
          "rule": "texture.max_size", "severity": "error", "title": "Too large",
          "what": "4096×4096. Prop textures may be at most 2048×2048.",
          "why": "On Quest a texture this size takes 16 MB of memory…",
          "how": "Scale the image down to 2048×2048 in Photoshop (Image > Image Size) and save it again.",
          "note": "Quest budget for props.",
          "measured": { "width": 4096, "height": 4096 }, "limit": { "max": 2048 },
          "objects": [], "fix": null
        }
      ]
    }
  ]
}
```

---

## 4. Dependencies on other plans

- **`fire-decode-plan.md`** - `fire-cli` becomes another consumer inside fire's repo. It does not need
  the extraction. Two of that plan's items help it:
  - `probe()` (FD19), for speed;
  - the `source` description (FD4), for bit depth and colour facts.

  It also needs three small fire-decode additions, none blocking:
  - expose the sRGB test;
  - report palette-PNG bit depth correctly;
  - a distinct "PSD has no merged image" error.
- **3D Review** - the `3d-review-cli` changes in AC13.
- **Launcher** - the three changes in AC14.

---

## 5. Rollout and gates

Each step lands only when its gate passes.

| Step | Work | Gate |
| --- | --- | --- |
| 0 | **Spikes** (section 6), with Fork on Windows | Each spike answered, and the plan updated where an answer changes it |
| 1 | **`asset-rules`**: `rules.json` v1, the resolution logic, results v1, git helpers, cache, `rules check` / `rules explain`, with test cases | Fixtures cover: first-match profiles, path narrowing, no match, each stage source in precedence order, a missing stage file, LFS and partially staged files |
| 2 | **`fire-cli`** (`check`, `info`, the v1 rules) and the three fire-decode additions. **`3d-review-cli`** (`--rules`, `--results` with what/why/how, `--staged`, `--files-from`, `note`) | Both CLIs give the same file the same profile and stage. Results validate against the schema. A 20-asset commit is checked in a few seconds; a cached one almost instantly |
| 3 | **Launcher**: results table control, window `--set`, the window exit code | A sample results file renders correctly. Row buttons see their row's fields. The hook reads the chosen exit code |
| 4 | **The rules repo**: `rules.json`, the first profiles and rules files, hook scripts, `asset-check.launcher`, `setup.launcher`, the Fork command | Setup on a clean Windows machine. A bad asset opens the window; a clean commit shows nothing; "Don't commit" cancels the commit and "Commit anyway" lets it through. The Fork command works |
| 5 | **Pilot** with 2-3 artists, in advise mode | Every finding is understood without help. False positives are fixed or the rule is tuned. Commit times are acceptable |
| 6 | **Unity editor tool** (AC17) | Imports of bad assets show findings. A pull shows nothing. Hook installation works on project open |
| 7 | **GitHub check** (AC18), reporting only | Findings on a pull request match the local results |
| 8 | **Enforce mode** (`mode: enforce`), and the GitHub check made required | Errors block, both locally and on pull requests |
| 9 | **Fixes**: `fix` filled in, the window's **Fix** buttons, `fire-cli fix`, `3d-review-cli optimize` | Each fix is validated on the pilot corpus before it is offered |

---

## 6. Spikes and open items

**Spikes (step 0):**
- **S1 - Fork and the hook on Windows:**
  - Does a GUI program started from the `sh` hook keep the commit waiting until it exits?
  - Can it come to the front?
  - What does Fork show while the hook runs, and when it exits 1?
  - Which PATH does the hook get?
  - How long can a hook take before Fork complains?
- **S2 - Sharing Fork custom commands through the repository:** where the file must live, and whether the
  command can run an `sh` script with the repository path.
- **S3 - git plumbing:** staged blob ids, the LFS object path for a staged file, detecting partially staged
  files, and the branch in a detached HEAD.

**Open items:**
- **Per-asset stage marking** (AC7): the mechanism is undecided.
- **Exceptions:** v1 doesn't block, so a reviewed exceptions list for individual files can wait until
  enforce mode.
- **Name of the shared crate and its repository:** `asset-rules` is a working name.

---

## Appendix A - Facts this plan rests on

As of November 2026.

**`3d-review-cli`** (3D Review 0.8.0, `docs/cli.md`; called `review-cli` up to 0.8.0, renamed in November 2026)
- **Commands:** `audit`, `optimize`, `info`.
- **Exit codes:** 0 OK, 1 anything else (including a finding at `--fail-on` or worse), 2 usage error.
- **Rules and output:**
  - `--profile unity|unreal|generic|<file.json>`.
  - `--report` writes JSON (`3d-review.cli-report` v1) that names each offending object.
  - 43 checks, with per-profile thresholds and severities.
  - Profiles are saved from the viewer or written by hand, and loaded leniently.
- **Text:** each check's what / why / how-to-fix text exists (`labels-audit.ftl`), but the CLI prints
  only a summary line per rule and the JSON has no text.
- **What it doesn't have:**
  - no per-project discovery of a profile;
  - no waivers;
  - no `@listfile`;
  - automatic fixes only for triangle count and LODs (`optimize --lods/--reduce --replace-models`);
  - no dry run.
- **Inputs:** FBX only. Git LFS pointers are detected ("Run git lfs pull").
- **Installed:** beside `3d-review.exe`; the PATH option is unticked by default, and the folder is
  findable through the registry's `InstallLocation`. On macOS, an optional `/usr/local/bin/3d-review-cli`
  symlink.

**Fire and fire-decode**
- **Fire:** the release build is a Windows GUI program with no console; it takes only a file path (plus
  two thumbnail maintenance flags).
- **What fire-decode reports:** dimensions, source bit depth, channels, alpha-present-but-opaque, ICC
  profile bytes, format label, DDS mips and layout, PSD layer list (`PsdLayers`).
- **What it doesn't, yet:**
  - a header-only path;
  - an sRGB test for the ICC profile (it exists but is private);
  - correct bit depth for palette PNGs (they report 8);
  - EXIF beyond orientation.

**Launcher** (`launcher-v2`, psmyles/launcher 0.2.x)
- **Tools and steps:**
  - `.launcher` tools in KDL: forms, steps, a live console, Lua.
  - Steps include `files.show-in-folder` (Explorer `/select`, Finder `open -R`) and `files.open`.
  - A `review3d.audit` add-on already wraps 3d-review-cli.
- **What it doesn't have:**
  - no list, table or per-row-button control;
  - the window mode takes only a file, though environment variables reach the tool's Lua;
  - the window always exits 0;
  - no single instance on Windows.
- **What it can already do:**
  - a debounced file watcher (used to reload `.launcher` files);
  - taskbar flash and notifications on Windows, Dock bounce on macOS.
- **Installed:** per user to `%LOCALAPPDATA%\Programs\Launcher\launcher.exe`; on macOS,
  `/Applications/Launcher.app`.

**bite-cli** (for later fixes)
- **Runs saved workflows headlessly**, with resize, strip-metadata and format-conversion nodes.
- **Gaps:**
  - input folders only;
  - a Gate failure still exits 0;
  - PNG output is always RGBA, so it can't remove alpha;
  - no colour-profile conversion.

**UnityTools**
- **`TextureCompressionChecker`** lists textures that aren't Crunch-compressed and applies an import
  Preset.
- **Nothing else** validates import settings or source images, and nothing reads `.meta` files directly.

**Fork**
- **Custom commands** are started by hand: from context menus, the toolbar's "Open in" dropdown, or Quick
  Launch.
  - They can run `sh` scripts or processes.
  - They can show simple dialogs (Fork 1.85).
  - They can be shared with the team (Fork 1.54).
- **No event trigger** for custom commands turned up.
- **Fork runs git's `pre-commit` hook.**
- Sources: [Fork release notes (Windows)](https://git-fork.com/releasenoteswin),
  [custom commands gist](https://gist.github.com/oktomus/e284923a7fce837615bb756a1060e5dc),
  [Tracker #287](https://github.com/fork-dev/Tracker/issues/287),
  [Tracker #1131](https://github.com/fork-dev/Tracker/issues/1131),
  [Tracker #948](https://github.com/fork-dev/Tracker/issues/948).
