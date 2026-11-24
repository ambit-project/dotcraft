<p align="center">
  <img src="assets/icon.png" width="112" height="112" alt="DotCraft application icon">
</p>

<h1 align="center">DotCraft</h1>

<p align="center">
  <strong>A lightweight, native IDE for .NET and C#.</strong><br>
  Solutions, Roslyn code intelligence, build, run, tests, debugging and Git in a fast desktop app built with Rust and egui.<br>
  macOS · Windows · Linux
</p>

<p align="center">
  <a href="LICENSE-MIT"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-512bd4?style=flat-square"></a>
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-253b53?style=flat-square&logo=rust">
  <img alt=".NET 8 and later" src="https://img.shields.io/badge/.NET-8%20%C2%B7%209%20%C2%B7%2010-512bd4?style=flat-square&logo=dotnet">
</p>

<p align="center">
  <a href="#highlights">Highlights</a> ·
  <a href="#requirements">Requirements</a> ·
  <a href="#build-from-source">Build</a> ·
  <a href="#keyboard">Keyboard</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#license">License</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/workspace-light.png">
    <img src="docs/screenshots/workspace-dark.png" alt="The DotCraft window with the solution explorer, a C# editor and the tool window stripes" width="100%">
  </picture>
  <br>
  <sub>A solution open in DotCraft. Light and dark themes follow the system.</sub>
</p>

DotCraft takes its layout and keyboard from JetBrains Rider's New UI — islands, tool window stripes, Search Everywhere, the Rider keymap — and runs as a single native binary that starts in a moment and stays responsive on large files. Screenshots are offscreen captures of the current source build on macOS with the sample solution in `tests/fixtures/Shop`.

## Highlights

| Edit | Build and debug | Navigate and review |
| --- | --- | --- |
| A custom editor with tree-sitter highlighting, folding, multi-caret editing and Roslyn semantic colors, completion, signature help, quick fixes, rename and inlay hints. | Build, run and test (xUnit, NUnit, MSTest) with the dotnet CLI; debug console and ASP.NET apps with netcoredbg; manage NuGet packages. | Search Everywhere, Find in Files, recent files and locations, an integrated terminal, and Git: changes, commit, branches, side-by-side diff, gutter marks and blame. |

### Code intelligence

DotCraft talks to the same C# language server as Visual Studio Code's C# extension (`Microsoft.CodeAnalysis.LanguageServer`). It finds an installed copy or downloads it on first use. Completion, hover, go to definition and implementations, find usages, cross-project rename, code actions and diagnostics come from Roslyn; syntax colors and structure come from tree-sitter, so the editor is useful before the server is ready.

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/completion-dark.png" alt="Completion popup with Roslyn items and documentation"></td>
    <td width="50%"><img src="docs/screenshots/search-dark.png" alt="Search Everywhere finding files by their initials"></td>
  </tr>
  <tr>
    <td align="center"><sub>Completion with documentation.</sub></td>
    <td align="center"><sub>Search Everywhere (⇧⇧) with fuzzy matching.</sub></td>
  </tr>
</table>

### Formatting

**Reformat Code** (⌥⌘L / Ctrl+Alt+L) formats C# with the Roslyn language server or with [CSharpier](https://csharpier.com) (1.0 or later). **Settings › Tools › C# code formatter** chooses: by default CSharpier is used when the repository uses it (CSharpier in its `dotnet-tools.json` or a `.csharpierrc`/`.csharpierignore`), and Roslyn otherwise. DotCraft runs the local tool (`dotnet csharpier`), the executable set in Settings › Tools, or a global `csharpier`, piping the file with `--stdin-path` so its options and ignore files apply. CSharpier formats whole files; **Reformat C# files with CSharpier on save** formats them on Save and Save All. JSON and XML are formatted by DotCraft itself.

### Run, test and debug

Run configurations are created from the solution: one per executable project and launch profile, plus the tests of each test project. Build errors go to the Problems window with links to the code; test results show in a tree and as gutter icons next to each test. Repositories whose `global.json` sets `"test": { "runner": "Microsoft.Testing.Platform" }` (xUnit v3, MSTest or NUnit on .NET 10+) run with the platform's options and TRX report; debugging a single test is not available in that mode yet.

In the explorer, the solution's context menu builds, rebuilds, cleans and restores it and runs all its tests; a project's menu does the same for the project, adds Run and Debug for web, worker and console projects, and Run Tests for test projects.

The explorer has two tabs. **Solution** shows the solution's projects; **Files** shows every folder and file of the git repository from its root, also when the solution lies in a subfolder (the workspace folder outside git), without `.git`, `bin` and `obj`. Projects and solutions are plain files there. A Markdown file's context menu has **Open Preview**, which opens it showing only the preview. The tab you pick is kept per workspace.

The debugger uses [netcoredbg](https://github.com/Samsung/netcoredbg) (MIT), downloaded on first use. It supports line and conditional breakpoints, step over/into/out, Run to Cursor, threads and frames, a lazily expanded variables tree, watches, Evaluate Expression, values on hover and stopping on exceptions.

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/debug-dark.png" alt="Paused on a conditional breakpoint with frames, watches and variables"></td>
    <td width="50%"><img src="docs/screenshots/tests-dark.png" alt="Test results of xUnit, NUnit and MSTest projects"></td>
  </tr>
  <tr>
    <td align="center"><sub>Paused on a conditional breakpoint.</sub></td>
    <td align="center"><sub>Tests from three frameworks in one run.</sub></td>
  </tr>
</table>

### Git

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/diff-dark.png" alt="Side-by-side diff of a changed file"></td>
    <td width="50%"><img src="docs/screenshots/blame-dark.png" alt="Blame annotations in the editor gutter"></td>
  </tr>
  <tr>
    <td align="center"><sub>Side-by-side diff with the editor's highlighting.</sub></td>
    <td align="center"><sub>Blame in the gutter.</sub></td>
  </tr>
</table>

### Unity projects

Open the folder of a Unity project (the one with `Assets` and `ProjectSettings`) with **File › Open…**. DotCraft opens the `.sln` Unity generated, and the C# code intelligence uses Unity's generated projects. Unity writes those files only for the editors of its IDE packages: if they are missing or out of date, choose Visual Studio or Visual Studio Code in Unity's **Preferences › External Tools**, click **Regenerate project files**, then choose DotCraft again. DotCraft reminds you of this when a Unity folder has no solution or projects yet.

To open Unity's scripts in DotCraft (double-click in the Project window or the Console), choose DotCraft as **External Script Editor** in **Preferences › External Tools** and set **External Script Editor Args** to `--unity $(ProjectPath) $(File) $(Line) $(Column)`. On macOS and Linux a running DotCraft window opens the file at that line.

Unity's cache folders at the project root (`Library`, `Temp`, `Logs`, `MemoryCaptures`) are left out of the explorer, Search Everywhere, Go to File, Find in Files and file watching, and the explorer hides the `.meta` file next to each asset. Renaming or deleting an asset in the explorer renames or deletes its `.meta` file too, so Unity keeps its references. **Show All Files** shows the hidden entries dimmed.

**Tools › Open in Unity Editor** starts the editor version recorded in `ProjectSettings/ProjectVersion.txt`, so opening never upgrades the project. DotCraft looks in Unity Hub's install folders (`/Applications/Unity/Hub/Editor` on macOS, `C:\Program Files\Unity\Hub\Editor` on Windows, `~/Unity/Hub/Editor` on Linux, and the custom location set in Unity Hub). **Settings › Tools › Unity Editor** takes an editor executable (or `Unity.app`), used for every project, or a folder of editors named by version, searched first.

Play mode, building players and debugging are done in the Unity Editor: Unity runs scripts on Mono, which DotCraft's .NET debugger (netcoredbg) cannot attach to.

### PostgreSQL files

`.sql`, `.pgsql` and `.psql` files are read by PostgreSQL 18's own parser (libpg_query), with no database connection, the way [PG Studio](https://github.com/ambit-project/ambit-pg-studio) does it:

- Highlighting from PostgreSQL's scanner, also inside `$$` function bodies; Liquibase `--changeset` headers stand out.
- Syntax errors and PL/pgSQL errors are underlined at the word and listed in **Problems** while you type.
- Hover a table, view, function, procedure or type: its signature, the file and changeset that create it, the changeset comment, and a table's columns after every `ALTER TABLE`.
- **Go to Declaration** (⌘/Ctrl-click) and **Find Usages** work across every SQL file of the workspace. Names resolve like PostgreSQL resolves them: through `SET search_path` (also a function's own), overloads by the number of arguments, and names inside PL/pgSQL bodies and `EXECUTE format('…')`. A usage says what it does: read, write, call, type, foreign key, index or trigger.
- **Structure** lists the objects a file creates, and Search Everywhere finds every object of the workspace.

The workspace is indexed in the background when it opens (900 files take about a fifth of a second) and again for each SQL file that changes on disk; open files count as edited.

**New Changeset…** (the explorer's and a SQL editor's context menu, inside a Liquibase `db/` folder) creates what a project's `db/scripts/new-ddl.sh`, `new-data.sh` and `new-module-routine.sh` do: a DDL or data changeset with today's next number (`changelog/<yyyy>/1-ddl/<yyyymmdd>-<nn>-<slug>.sql`, data optionally `context:dev`), or a module's view, procedure or function (`changelog/modules/<nn>-<module>/<kind>s/<schema>.<name>.sql`, `runOnChange:true`, switching to the module owner's role), with its `CHANGELOG.md` entry. **Module** adds another module: `changelog/modules/<nn>-<module>/` (the next free number after the highest below 90) and the DDL changeset of its schemas (`<module>`, `<module>_app`, optionally `<module>_public`, `<module>_internal`, owned by the module owner, the contract's usage and default privileges for the API's role, with rollbacks). The author and the owner's role are read from the project's existing changesets (`CHANGESET_AUTHOR` overrides the author), so it works in projects without the scripts too.

## Requirements

- macOS 11 or later (Apple Silicon or Intel), Windows 10/11 (x64 or ARM64, DirectX 12), or Linux with glibc 2.35+ (x64 or ARM64; Vulkan or OpenGL ES, X11 or Wayland).
- The [.NET SDK](https://dotnet.microsoft.com/download) 8 or later. DotCraft looks in the default install locations and on `PATH`; another location can be set in **Settings › Tools**.
- Git on `PATH` for the Git features.
- For Unity projects, optionally the Unity Editor installed with Unity Hub (to use **Open in Unity Editor**).
- An internet connection the first time you open a C# file or start debugging, unless the language server and netcoredbg are already installed (their paths can be set in **Settings › Tools**).

The interface is available in English, Spanish, German, French, Japanese, Polish, Brazilian Portuguese and Simplified Chinese.

## Build from source

DotCraft needs Rust 1.90 or later (edition 2024).

```bash
cargo run --release -p rc-app
```

Packages for distribution:

```bash
scripts/bundle.sh --universal --dmg --zip
```

```bash
scripts/bundle.sh --windows
```

The first builds `DotCraft.app`, a universal `.dmg` and a `.zip` in `target/release`; the second cross-compiles the Windows x64 and ARM64 executables with [cargo-xwin](https://github.com/rust-cross/cargo-xwin) and zips them. Linux builds come from the `Linux release build` workflow (`.github/workflows/linux.yml`), which runs on Ubuntu 22.04 when a release is published and attaches `DotCraft-<version>-linux-x64.tar.gz` and `-linux-arm64.tar.gz`; on Linux, `cargo build --release` is enough. On Debian and Ubuntu, `scripts/deb.sh` builds `target/release/dotcraft_<version>_<arch>.deb` (the binary, desktop entry and icons; `--install` installs it with apt). Application icons are generated from `assets/icon.svg` with `scripts/make_icon.py`.

### Tests

```bash
cargo test --workspace
```

Integration tests run against the real .NET SDK, the Roslyn language server and netcoredbg (downloaded once into `target/test-home`) using the solutions in `tests/fixtures`. Window captures are rendered offscreen with the GPU and written to `target/snapshots`:

```bash
cargo test -p rc-app -- --ignored
```

## Keyboard

DotCraft ships the Rider keymap for macOS and for Windows/Linux, and a Visual Studio keymap (**Settings › Keymap**). Some of the defaults:

| Action | macOS (Rider) | Windows (Rider) |
| --- | --- | --- |
| Search Everywhere | ⇧⇧ or ⌘T | Shift Shift or Ctrl+T |
| Go to File / Type / Symbol | ⇧⌘O / ⌘O / ⌥⇧⌘O | Ctrl+Shift+N / Ctrl+N / Ctrl+Alt+Shift+N |
| Quick fixes | ⌥↩ | Alt+Enter |
| Rename | ⇧F6 | Shift+F6 |
| Build / Run / Debug | ⌘F9 / ⌃R / ⌃D | Ctrl+Shift+F9 / Ctrl+F5 / F5 |
| Step over / into / out | F8 / F7 / ⇧F8 | F10 / F11 / Shift+F11 |
| Toggle breakpoint | ⌘F8 | F9 |
| Commit | ⌘K | Ctrl+K |

## Architecture

DotCraft is a Cargo workspace; each crate has one job:

| Crate | Purpose |
| --- | --- |
| `rc-app` | The application: window, tool windows, dialogs, wiring |
| `rc-kit` | Theme (Islands), icons, window chrome, i18n |
| `rc-editor` | Editor widget, keymaps, popups |
| `rc-text` | Rope buffer, selections, edits and undo |
| `rc-syntax` | tree-sitter parsing, highlighting, folding and structure |
| `rc-lsp` | LSP client and the Roslyn language server |
| `rc-dap` | DAP client and netcoredbg sessions |
| `rc-dotnet` | dotnet CLI: solutions, projects, build, run, tests, NuGet |
| `rc-vcs` | Git through the `git` CLI |
| `rc-search` | File index, fuzzy search and Find in Files |
| `rc-sql` | PostgreSQL files: PostgreSQL's parser (libpg_query), definitions, references, index |
| `rc-term` | Terminal emulator (alacritty_terminal) |
| `rc-core` | Settings, sessions, workspace, background work |

See [docs/architecture.md](docs/architecture.md) and [docs/performance.md](docs/performance.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Icons in `assets/icons` come from the [IntelliJ Platform](https://github.com/JetBrains/intellij-community) (Apache 2.0); see [assets/icons/NOTICE.md](assets/icons/NOTICE.md). DotCraft is not affiliated with JetBrains or Microsoft.
