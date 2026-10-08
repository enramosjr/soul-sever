# Agent Governance & OKF Synchronization Protocol

This document establishes the mandatory governance standards, workflow gates, evidence rules, and persona/agent selection protocols for all AI agents (coding assistants, autonomous subagents, and human-agent pair programmers) operating within this repository.

---

## 1. Core Mandate: Open Knowledge Format (OKF) Synchronization

All AI agents working on this project **must** ensure that the Open Knowledge Format corpus (`knowledge/`) remains synchronized with any code, specification, architecture, or workflow changes introduced during execution.

### Rationale & Purpose
The OKF corpus (`knowledge/`) is the machine-readable, persistent knowledge graph of the repository. It serves as the primary context for future AI agents and human contributors. When codebase changes occur without corresponding OKF updates, agent drift and knowledge decay occur.

- **Requirement**: Any agent change that alters functionality, contracts, specifications, dependencies, or architectural patterns **must** update the corresponding OKF document(s) and append a log entry to `knowledge/log.md`.
- **Scope**: Code changes are not considered complete until OKF synchronization is verified and updated where relevant.
- **Bootstrap**: If `knowledge/` does not exist yet, create it with `python3 .ai/scripts/okf.py init` before the first OKF update.

---

## 2. Mandatory Persona & Agent Context Selection (`.ai/` Integration)

All AI agents working on this codebase **must** automatically detect the active language, file context, and task role, check the `.ai/` directory, and apply the corresponding principal engineering persona and agent guidelines. This document is the repository-level authorization for automatic persona selection referenced by the agent role files.

### 2.1 Language & Domain Persona Mapping

When inspecting, editing, or reviewing files, agents must load the matching persona from `.ai/personas/`:

| File Context / Extension | Target Language / Surface | Persona File |
|--------------------------|---------------------------|--------------|
| `.rs`, `Cargo.toml` | Rust | [`.ai/personas/rust-principal-engineer.md`](.ai/personas/rust-principal-engineer.md) |
| `.py`, `.pyi`, `.ipynb`, `pyproject.toml` | Python | [`.ai/personas/python-principal-engineer.md`](.ai/personas/python-principal-engineer.md) |
| `.js`, `.mjs`, `.cjs`, `.ts`, `.mts`, `.cts`, `.jsx`, `.tsx`, `package.json`, `tsconfig.json` | JavaScript & TypeScript | [`.ai/personas/javascript-typescript-principal-engineer.md`](.ai/personas/javascript-typescript-principal-engineer.md) |
| `.go`, `go.mod` | Go | [`.ai/personas/go-principal-engineer.md`](.ai/personas/go-principal-engineer.md) |
| `.cs`, `.csproj`, `.sln` | C# | [`.ai/personas/csharp-principal-engineer.md`](.ai/personas/csharp-principal-engineer.md) |
| `.dart`, `pubspec.yaml` | Dart & Flutter | [`.ai/personas/dart-flutter-principal-engineer.md`](.ai/personas/dart-flutter-principal-engineer.md) |
| `.swift`, `Package.swift` | Swift & SwiftUI | [`.ai/personas/swift-swiftui-principal-engineer.md`](.ai/personas/swift-swiftui-principal-engineer.md) |
| Any other language or surface | No dedicated persona | Follow the repository's existing conventions and the agent role file alone. |

### 2.2 Task Role & Agent Selection

Depending on the operational stage of the task, agents must align with the appropriate role definition in `.ai/agents/`:

| Task Stage / Workflow | Agent Role File | Core Focus |
|-----------------------|-----------------|------------|
| Planning, codebase investigation, and architectural design | [`.ai/agents/planner.md`](.ai/agents/planner.md) | Read-only discovery, factual trade-offs, implementation-ready plans |
| Plan execution, coding, refactoring, bug fixes, test implementation | [`.ai/agents/implementer.md`](.ai/agents/implementer.md) | Surgical edits, minimal code, task plan execution, validation |
| Reviewing plans, designs, diffs, or completed work | [`.ai/agents/reviewer.md`](.ai/agents/reviewer.md) | Evidence-based findings, severity, validation quality |

### 2.3 Operational Loading Rules

1. **Context Identification**: Upon receiving a task or operating on files, identify the applicable task role and language persona.
2. **Selective Segment Loading**: Read the matching persona document and extract only the guidance segments pertinent to the immediate task.
3. **Multi-Stack Tasks**: For tasks spanning several languages, use one primary persona for the area that dominates implementation risk and apply others only within their files, as described in the agent role files.

---

## 3. OKF Structure Overview

The OKF bundle resides in the `knowledge/` directory and follows the OKF v0.2 layout. `okf.py init` creates the root files and the standard sections below:

| Path | Purpose | Update Trigger |
|------|---------|----------------|
| `knowledge/index.md` | Entry point & root index for the OKF bundle | Adding or removing root bundle sections |
| `knowledge/log.md` | Chronological log of all OKF updates | **Mandatory on every OKF update** |
| `knowledge/project/` | Architecture, development setup, evidence rules, boundaries | Architectural shifts, directory re-organizations, boundary/policy updates |
| `knowledge/capabilities/` | What the product does: features and user-visible behavior | Adding, modifying, or deprecating capabilities |
| `knowledge/specifications/` | Normalized views of product contracts | Updating product contracts or implementation guidance |
| `knowledge/history/` | Dated implementation arc and delivery milestones | Updating implementation history or milestones |
| `knowledge/concepts/` | Domain concepts, data models, or protocol definitions | Introducing new core domain models or protocol definitions |
| `knowledge/playbooks/` | Operational, setup, and troubleshooting playbooks | Adding or revising developer or deployment operational guides |

### 3.1 OKF Tooling (`.ai/scripts/okf.py`) — Script First

Any OKF operation the script covers **must** go through it instead of reading, grepping, or hand-editing `knowledge/`. It is deterministic, stdlib-only (Python 3.9+), and prints compact output. Read a full document only when you need its prose.

| Need | Command (`python3 .ai/scripts/okf.py …`) |
|------|------------------------------------------|
| Create the bundle skeleton | `init` |
| Discover docs (one line each) | `list [--type T] [--tag T] [--status S] [--under DIR] [--long]` |
| Read cheaply | `show PATH` (frontmatter + outline with line numbers), `--section HEADING`, `--meta`, `--body` |
| Find text across the bundle | `search PATTERN [--regex] [--limit N]` |
| Which docs a change affects | `affected [PATHS…]` (default: git changes vs `HEAD`; matches `sources[].resource`) |
| New concept doc with full frontmatter | `new PATH --type T --title X --description D [--tags a,b] [--source path[=Title]]…` |
| Edit frontmatter in place | `set PATH key=value…`, `stamp verified PATH… --by "process:<command>"` |
| Log entry (dated, newest-first) | `log "message" [--date YYYY-MM-DD]` |
| Regenerate `index.md` listings | `reindex [--check]` (only rewrites the `okf:index` marker block) |
| Gate 4 structural check | `validate [--strict] [--json]` (exit 1 on errors) |

Path conventions follow the OKF SPEC (§6.1, §6.2): links and path fields are either URLs, bundle-relative paths beginning with `/`, or paths relative to the containing document. `new --source` and `affected` take **repo-relative** paths and convert them for you. Set `OKF_ACTOR` (e.g. `agent/<model-id>`) so `generated.by` is attributed. `stamp verified` records only the actor you pass, so stamp only after the cited command actually ran and passed (§8.5). Tests: `python3 -m unittest discover -s .ai/scripts/tests`.

---

## 4. OKF Relevance Trigger Matrix

`okf.py affected` is the first-pass detector: it maps changed files to the docs that cite them in `sources` and lists changed files no doc covers. Agents must also check their changes against this matrix, which covers changes that `sources` mappings cannot see:

| Category of Change | Typical Workspace Paths | Required OKF Action | OKF Target Section |
|--------------------|-------------------------|---------------------|--------------------|
| **User-visible behavior** | Feature code, UI, CLI commands, endpoints | Add or update the capability doc | `knowledge/capabilities/` |
| **Public contracts** | API definitions, schemas, wire formats, config formats, exported interfaces | Update the specification mirror | `knowledge/specifications/` |
| **Architecture & boundaries** | New, moved, or removed modules, packages, services; dependency direction | Update architecture and boundary docs | `knowledge/project/` |
| **Build, toolchain, dependencies** | Build files, lockfiles, CI configuration, developer scripts | Update the development guide and setup playbooks | `knowledge/project/`, `knowledge/playbooks/` |
| **Operational procedures** | Deployment, release, migration, troubleshooting steps | Add or revise the playbook | `knowledge/playbooks/` |
| **Domain models** | Core entities, data models, protocols | Add or update the concept doc | `knowledge/concepts/` |
| **Delivered milestones** | A completed, verified feature slice | Record the milestone | `knowledge/history/` |
| **Any OKF update** | `knowledge/**` | Append change entry | `knowledge/log.md` |

Vendored code or submodules that carry their own OKF bundle should have behavior changes recorded in that bundle; this bundle tracks only the integration boundary.

---

## 5. OKF Document Standards & Evidence Hierarchy

### 5.1 Required YAML Frontmatter (OKF v0.2)

Follow the public [OKF SPEC](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md) for reserved files and concept frontmatter:

| File | Frontmatter rule |
|------|------------------|
| Bundle-root `knowledge/index.md` | MAY contain **only** `okf_version: "0.2"` (SPEC §8 / §12) |
| Any other `index.md` | **No** frontmatter (directory listing body only) (SPEC §8) |
| Any `log.md` | **No** frontmatter; date headings `## YYYY-MM-DD`, newest first (SPEC §9) |
| All other `.md` concept documents | **Required** parseable YAML frontmatter with non-empty `type` (SPEC §4 / §11) |

Concept documents SHOULD also carry the recommended and project-extension fields:

```yaml
---
type: Architecture | Capability | Specification Mirror | Concept | Historical Record | Playbook | Development Guide | Product Boundary
title: Short Descriptive Title
description: One sentence summary of the knowledge document.
tags: [tag1, tag2, tag3]
status: stable | draft | deprecated
evidence: implementation-and-current-specifications | project-documentation | validated-runtime
generated: { by: "agent-name/model-id", at: 2026-07-27T00:00:00Z }
verified: { by: "process:test-suite", at: 2026-07-27T00:00:00Z }
sources:
  - id: source-identifier
    resource: ../../src/module/file.ext
    title: Human-readable source description
---
```

`type` values are producer-defined, not centrally registered (SPEC §4.1). `evidence` is a repository extension key; consumers must tolerate unknown keys (SPEC §4.1). `verified` may be a single `{ by, at }` map or a list of them (SPEC §5.2). Actor strings in `generated.by` / `verified.by` follow SPEC §7: `<producer>/<version>` for agents and tools, `human:<id>` for people (required for human-authored or human-confirmed content), `process:<id>` for automated processes.

### 5.2 Strict Evidence Hierarchy
Claims in OKF documents must adhere to this evidence hierarchy:

1. **Implemented source** (application and library code, scripts) — Strongest evidence for shipped behavior.
2. **Executable configuration** (build files, CI configuration, lockfiles, schemas, infrastructure definitions) — Strong evidence for toolchain and build behavior.
3. **Validated build/runtime output** (test runs, built artifacts, deployment or runtime logs) — Verification evidence.
4. **Current project documentation** — Useful context, but not proof of behavior without source or runtime validation.

> **Rule**: Agents must never upgrade a spec requirement or planned feature into a release claim without verifying source code implementation.

---

## 6. Mandatory Agent Execution Gates

Every agent executing a task in this repository must pass through the following 4 gates:

```
  ┌─────────────────────────────────────────────────────────────┐
  │ Gate 1: Pre-Task Discovery & Persona Selection              │
  │ • Read AGENTS.md, knowledge/index.md, & target OKF docs     │
  │ • Detect file context & load relevant .ai/ persona/agent    │
  └──────────────────────────────┬──────────────────────────────┘
                                 │
                                 ▼
  ┌─────────────────────────────────────────────────────────────┐
  │ Gate 2: In-Flight Impact Evaluation                         │
  │ • Track code, spec, and architectural modifications         │
  │ • Apply loaded persona guidelines to code changes           │
  └──────────────────────────────┬──────────────────────────────┘
                                 │
                                 ▼
  ┌─────────────────────────────────────────────────────────────┐
  │ Gate 3: OKF Synchronization Gate                            │
  │ • Evaluate relevance trigger matrix                         │
  │ • Update/create knowledge/ documents                        │
  │ • Append update log entry to knowledge/log.md               │
  └──────────────────────────────┬──────────────────────────────┘
                                 │
                                 ▼
  ┌─────────────────────────────────────────────────────────────┐
  │ Gate 4: Final Verification & Sign-Off                       │
  │ • Run the repository's build and test commands              │
  │ • Verify OKF links, YAML frontmatter, and evidence integrity│
  └─────────────────────────────────────────────────────────────┘
```

### Gate 1: Pre-Task Discovery & Persona Selection
Before making edits:
1. Inspect `knowledge/index.md` and relevant `knowledge/` subdirectories to understand current specifications and architecture (`okf.py list`, then `okf.py show PATH [--section …]`).
2. Identify the active file context and load the corresponding persona from `.ai/personas/` and task role from `.ai/agents/` (Planner, Implementer, or Reviewer).

### Gate 2: In-Flight Impact Evaluation
As you modify source, configuration, scripts, or specification files:
1. Identify which OKF capabilities, specs, architectural docs, or history items are affected by your changes.
2. Apply the loaded persona's engineering philosophy, type guidelines, and code quality standards to all changes.

### Gate 3: OKF Synchronization Gate (Pre-Completion Check)
Before presenting a task as completed or ready for review, ask:
1. *Did my changes alter product capabilities, specs, architecture, or project history?*
2. *Is the OKF corpus (`knowledge/`) up-to-date with this change?*

Run `okf.py affected` to list docs whose sources your change touches, plus changed files no doc covers.

If the corpus is not up to date, perform the following actions:
- Create or update the necessary `.md` files in `knowledge/` (`okf.py new` / `set` / `stamp`; `reindex` runs automatically after `new` and `set`).
- Ensure concept documents have complete YAML frontmatter (`type`, recommended trust/provenance fields, and repo `evidence` / `sources`). Do not add concept frontmatter to reserved `index.md` / `log.md` files.
- Append a dated entry to `knowledge/log.md` describing what knowledge was added, updated, or verified (`okf.py log "…"`).

### Gate 4: Final Verification & Sign-Off
Verify that:
- Relevant builds succeed using the repository's own build commands.
- **Applicable end-to-end tests for the changed slice actually pass** (not merely that a script exists). See §8.
- All relative file links inside updated OKF documents resolve correctly (`okf.py validate` exits 0; it also checks frontmatter, log ordering, index freshness, and evidence claims).
- No unverified or speculative claims are presented as shipped functionality.
- OKF `verified` / log entries name the **exact commands and outcomes** used as evidence (e.g. `<test command>` → PASS), never “assumed green” or “to be verified later.”

---

## 7. Enforcement & Failure Remediation

If an agent or automated check detects that OKF is out of sync with current implementation:
1. Do not ignore the discrepancy.
2. Treat stale or missing OKF documentation as a build/completion blocker.
3. Update the affected OKF files, append a log entry in `knowledge/log.md`, and explain the documentation fix in the task summary.

---

## 8. Completeness & Testing Mandate (No Partial Slices)

Work that **looks** done — scripts, playbooks, “Verified …” log lines — while the real path fails, or that is backed by tests that pass without asserting the actual success criteria, is **forbidden**.

### 8.1 Absolute prohibitions

Agents **must not** land or claim complete any of the following:

| Forbidden | Examples |
|-----------|----------|
| Stubs / placeholders | `todo!()`, `unimplemented!()`, `pass # TODO`, `throw new NotImplementedException()`, empty function bodies that compile but do nothing |
| Deferred behavior presented as done | “future work”, “will wire later” left in the critical path of a claimed feature |
| Weak or misleading tests | Asserting “process started” or “file exists” while calling that an end-to-end test |
| Evidence inflation | Writing OKF/log “Verified …” without a command that failed closed on the real success criteria |
| Half-integrated paths | Interactive prompts on a path that must run non-interactively in CI; cleanup code that references uninitialized state |

If a slice cannot be finished in the current task, **do not merge the half**: either complete it with tests, or leave it uncommitted / behind an explicit unfinished branch — never document it as working.

### 8.2 Definition of done for a feature slice

A slice is done only when **all** of the following hold:

1. **Behavior works** on the intended path (CLI, service, UI, library, or package — whichever the feature claims).
2. **Unit tests** cover pure logic where it exists. Absence of unit-testable logic is fine; absence of *any* verification is not.
3. **End-to-end test** exercises the user-visible success criteria and **fails if those criteria are unmet**.
4. **OKF + `knowledge/log.md`** describe what was verified with concrete commands and outcomes.
5. No `TODO`/`FIXME`/stub remains on the critical path of that slice.

### 8.3 What counts as a real E2E

| Claim | Minimum acceptable E2E |
|-------|------------------------|
| CLI command works | Invoke the built command with realistic arguments; assert exit status, output, and resulting side effects |
| HTTP/RPC endpoint works | Call the running service; assert status, response body, and persisted state |
| UI flow works | Drive the UI through the flow (browser or device automation); assert the visible end state |
| Library/package works | Install the built artifact into a clean environment and exercise its public API |
| Data migration works | Run the migration against representative data; assert schema and data afterward, and rollback if supported |
| Deployment/installer works | Deploy or install into a clean target; assert the installed system reaches its expected running state |

Smoke checks that only prove startup are allowed as **additional** diagnostics, never as the sole proof for a behavioral claim.

### 8.4 Gate 4 test matrix (agents must run applicable rows)

Before sign-off, run every row that intersects the change, using the repository's actual commands (build files, package scripts, CI configuration):

| Touched area | Required verification |
|--------------|----------------------|
| Pure logic | Unit tests for the affected modules |
| Public interfaces, schemas, integrations | Integration or contract tests |
| User-visible flows | The applicable E2E from §8.3 |
| Build, packaging, dependencies | Clean build, and artifact installation where applicable |
| `knowledge/**` | `python3 .ai/scripts/okf.py validate` exits 0 |
| `.ai/scripts/**` | `python3 -m unittest discover -s .ai/scripts/tests` |

If the environment cannot run a required verification (missing hardware, privileges, services, or artifacts), **say so explicitly**, do not invent a PASS, and do not upgrade OKF evidence beyond what ran.

### 8.5 OKF evidence language

- Prefer `evidence: validated-runtime` only when an E2E in §8.3 actually passed in this change.
- `verified.by: process:repository-scan` means files exist — **not** that the feature works. Do not use scan-only verification to close behavioral claims.
- Log entries must quote failing-closed tests (e.g. `<test command>` → PASS) or state blockers honestly.

---

# Behavioral Guidelines

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.
