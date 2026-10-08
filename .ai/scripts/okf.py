#!/usr/bin/env python3
"""Deterministic operations on an Open Knowledge Format (OKF v0.2) bundle.

Agents call this instead of reading, indexing, editing, or validating
``knowledge/`` by hand. Standard library only; Python 3.9+.

Run ``python3 .ai/scripts/okf.py --help`` for the command list.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Iterator
from urllib.parse import unquote

OKF_VERSION = "0.2"
STANDARD_SECTIONS = (
    "project",
    "capabilities",
    "specifications",
    "history",
    "concepts",
    "playbooks",
)
RECOMMENDED_FIELDS = (
    "title",
    "description",
    "tags",
    "status",
    "evidence",
    "generated",
    "verified",
    "sources",
)
STATUS_VALUES = frozenset({"stable", "draft", "deprecated"})
EVIDENCE_VALUES = frozenset(
    {
        "implementation-and-current-specifications",
        "project-documentation",
        "validated-runtime",
    }
)
SCAN_ONLY_ACTOR = "process:repository-scan"
INDEX_BEGIN = "<!-- okf:index:begin -->"
INDEX_END = "<!-- okf:index:end -->"
LOG_TITLE = "# OKF Change Log"

_KEY_RE = re.compile(r"^([A-Za-z_][\w.-]*)[ \t]*:(?:[ \t]+(.*))?$")
_FIELD_NAME_RE = re.compile(r"^[A-Za-z_][\w.-]*$")
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_TIMESTAMP_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$"
)
_INT_RE = re.compile(r"^[-+]?\d+$")
_FLOAT_RE = re.compile(r"^[-+]?(?:\d+\.\d*|\.\d+|\d+)(?:[eE][-+]?\d+)?$")
_PLAIN_RE = re.compile(r"^[\w./][\w ./()+'-]*$")
_AMBIGUOUS_PLAIN = frozenset(
    {"true", "false", "yes", "no", "on", "off", "null", "~", "y", "n"}
)
_SCHEME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)(?:\s+#+)?\s*$")
_LINK_RE = re.compile(r"\[(?:[^\]\\]|\\.)*\]\(\s*<?([^)\s>]+)>?(?:\s+[\"'(][^)]*)?\)")


class OkfError(Exception):
    """User-facing failure, reported on stderr with exit status 1."""


class FrontmatterError(OkfError):
    """Frontmatter is missing a terminator or outside the supported YAML subset."""


# --------------------------------------------------------------------------
# YAML subset: the frontmatter shapes OKF documents actually use.
# Scalars, quoted strings, flow lists/maps, one-level block maps, and block
# lists of scalars or flat maps. Anything else is rejected, not guessed.
# --------------------------------------------------------------------------


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip())


def _strip_comment(raw: str) -> str:
    quote: str | None = None
    i = 0
    while i < len(raw):
        ch = raw[i]
        if quote:
            if quote == '"' and ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'" and (i == 0 or raw[i - 1] in " \t[{,:"):
            quote = ch
        elif ch == "#" and (i == 0 or raw[i - 1] in " \t"):
            return raw[:i].rstrip()
        i += 1
    return raw


def _split_flow(inner: str, line: int) -> list[str]:
    parts: list[str] = []
    buf: list[str] = []
    depth = 0
    quote: str | None = None
    i = 0
    while i < len(inner):
        ch = inner[i]
        if quote:
            buf.append(ch)
            if quote == '"' and ch == "\\" and i + 1 < len(inner):
                buf.append(inner[i + 1])
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'" and not "".join(buf).strip():
            quote = ch
            buf.append(ch)
        elif ch in "[{":
            depth += 1
            buf.append(ch)
        elif ch in "]}":
            depth -= 1
            buf.append(ch)
        elif ch == "," and depth == 0:
            parts.append("".join(buf).strip())
            buf = []
        else:
            buf.append(ch)
        i += 1
    if quote or depth:
        raise FrontmatterError(f"line {line}: unbalanced quotes or brackets")
    parts.append("".join(buf).strip())
    return [part for part in parts if part]


def parse_scalar(raw: str, line: int = 0) -> object:
    """Parse one inline YAML value (plain, quoted, or flow collection)."""
    s = raw.strip()
    if not s or s in ("~", "null", "Null", "NULL"):
        return None
    first = s[0]
    if first in "|>":
        raise FrontmatterError(f"line {line}: block scalars ('|', '>') are not supported")
    if first == '"':
        if len(s) < 2 or not s.endswith('"'):
            raise FrontmatterError(f"line {line}: unterminated double-quoted string")
        try:
            return json.loads(s)
        except json.JSONDecodeError as error:
            raise FrontmatterError(f"line {line}: invalid double-quoted string") from error
    if first == "'":
        if len(s) < 2 or not s.endswith("'"):
            raise FrontmatterError(f"line {line}: unterminated single-quoted string")
        return s[1:-1].replace("''", "'")
    if first == "[":
        if not s.endswith("]"):
            raise FrontmatterError(f"line {line}: unterminated flow list")
        return [parse_scalar(part, line) for part in _split_flow(s[1:-1], line)]
    if first == "{":
        if not s.endswith("}"):
            raise FrontmatterError(f"line {line}: unterminated flow map")
        mapping: dict[str, object] = {}
        for part in _split_flow(s[1:-1], line):
            key, sep, value = part.partition(":")
            if not sep:
                raise FrontmatterError(f"line {line}: flow map entry without ':': {part!r}")
            mapping[key.strip().strip("\"'")] = parse_scalar(value, line)
        return mapping
    if s in ("true", "True", "TRUE"):
        return True
    if s in ("false", "False", "FALSE"):
        return False
    if _INT_RE.match(s):
        return int(s)
    if _FLOAT_RE.match(s):
        return float(s)
    return s


def _inline_value(raw: str | None, line: int) -> object:
    return parse_scalar(_strip_comment(raw or ""), line)


def _parse_block_list(content: list[tuple[int, str]]) -> list[object]:
    items: list[object] = []
    dash_indent = _indent(content[0][1])
    current: dict[str, object] | None = None
    for lineno, line in content:
        indent = _indent(line)
        stripped = line.strip()
        if indent == dash_indent and (stripped == "-" or stripped.startswith("- ")):
            rest = stripped[1:].strip()
            match = None if rest[:1] in ("'", '"', "[", "{") else _KEY_RE.match(rest)
            if match:
                current = {match.group(1): _inline_value(match.group(2), lineno)}
                items.append(current)
            else:
                current = None
                items.append(_inline_value(rest, lineno))
            continue
        if current is None or indent <= dash_indent:
            raise FrontmatterError(f"line {lineno}: misplaced list content: {stripped!r}")
        match = _KEY_RE.match(stripped)
        if not match:
            raise FrontmatterError(f"line {lineno}: expected 'key: value' in list item")
        current[match.group(1)] = _inline_value(match.group(2), lineno)
    return items


def _parse_block(block: list[tuple[int, str]]) -> object:
    content = [
        (lineno, line)
        for lineno, line in block
        if line.strip() and not line.strip().startswith("#")
    ]
    if not content:
        return None
    if content[0][1].strip().startswith("-"):
        return _parse_block_list(content)
    mapping: dict[str, object] = {}
    base = _indent(content[0][1])
    for lineno, line in content:
        if _indent(line) != base:
            raise FrontmatterError(f"line {lineno}: nesting deeper than one level is not supported")
        match = _KEY_RE.match(line.strip())
        if not match:
            raise FrontmatterError(f"line {lineno}: expected 'key: value'")
        mapping[match.group(1)] = _inline_value(match.group(2), lineno)
    return mapping


def parse_frontmatter(text: str, first_line: int = 2) -> dict[str, object]:
    """Parse frontmatter text (without fences). ``first_line`` is its file line."""
    result: dict[str, object] = {}
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        line = lines[i]
        lineno = first_line + i
        if not line.strip() or line.lstrip().startswith("#"):
            i += 1
            continue
        if line[0].isspace() or line.startswith("-"):
            raise FrontmatterError(f"line {lineno}: unexpected indentation: {line.strip()!r}")
        match = _KEY_RE.match(line.rstrip())
        if not match:
            raise FrontmatterError(f"line {lineno}: expected 'key: value'")
        key, raw = match.group(1), match.group(2)
        if key in result:
            raise FrontmatterError(f"line {lineno}: duplicate key {key!r}")
        i += 1
        if raw is not None and _strip_comment(raw).strip():
            result[key] = _inline_value(raw, lineno)
            continue
        block: list[tuple[int, str]] = []
        while i < len(lines) and (
            not lines[i].strip() or lines[i][0].isspace() or lines[i].startswith("-")
        ):
            block.append((first_line + i, lines[i]))
            i += 1
        result[key] = _parse_block(block)
    return result


def dump_scalar(value: object) -> str:
    """Render a value in inline (flow) YAML form."""
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(dump_scalar(item) for item in value) + "]"
    if isinstance(value, dict):
        if not value:
            return "{}"
        return "{ " + ", ".join(f"{k}: {dump_scalar(v)}" for k, v in value.items()) + " }"
    text = str(value)
    if _TIMESTAMP_RE.match(text) or _DATE_RE.match(text):
        return text
    if (
        _PLAIN_RE.match(text)
        and not text.endswith(" ")
        and text.lower() not in _AMBIGUOUS_PLAIN
        and not _FLOAT_RE.match(text)
    ):
        return text
    return json.dumps(text, ensure_ascii=False)


def dump_entry(key: str, value: object) -> list[str]:
    """Render one top-level frontmatter entry as lines."""
    if isinstance(value, list) and any(isinstance(item, dict) for item in value):
        lines = [f"{key}:"]
        for item in value:
            if isinstance(item, dict) and item:
                for position, (k, v) in enumerate(item.items()):
                    prefix = "  - " if position == 0 else "    "
                    lines.append(f"{prefix}{k}: {dump_scalar(v)}")
            else:
                lines.append(f"  - {dump_scalar(item)}")
        return lines
    return [f"{key}: {dump_scalar(value)}"]


def split_frontmatter(text: str) -> tuple[str | None, str, int]:
    """Return (frontmatter text or None, body, 1-based file line of the body)."""
    lines = text.split("\n")
    if lines[0].rstrip() != "---":
        return None, text, 1
    for i in range(1, len(lines)):
        if lines[i].rstrip() in ("---", "..."):
            return "\n".join(lines[1:i]), "\n".join(lines[i + 1 :]), i + 2
    raise FrontmatterError("unterminated frontmatter (missing closing '---')")


def _top_level_key(line: str) -> str | None:
    if not line or line[0].isspace():
        return None
    match = _KEY_RE.match(line.rstrip())
    return match.group(1) if match else None


def _key_span(lines: list[str], key: str) -> tuple[int, int] | None:
    start = next((i for i, line in enumerate(lines) if _top_level_key(line) == key), None)
    if start is None:
        return None
    end = start + 1
    while end < len(lines):
        line = lines[end]
        if line.strip() and not line[0].isspace() and not line.startswith("-"):
            break
        end += 1
    while end > start + 1 and not lines[end - 1].strip():
        end -= 1
    return start, end


def update_frontmatter(text: str, updates: dict[str, object]) -> str:
    """Replace or append top-level keys, leaving every other line untouched."""
    fm, body, body_line = split_frontmatter(text)
    lines: list[str] = []
    if fm is not None:
        parse_frontmatter(fm)
        lines = fm.split("\n")
    else:
        body = text
    for key, value in updates.items():
        rendered = dump_entry(key, value)
        span = _key_span(lines, key)
        if span is None:
            while lines and not lines[-1].strip():
                lines.pop()
            lines.extend(rendered)
        else:
            lines[span[0] : span[1]] = rendered
    return "---\n" + "\n".join(lines) + "\n---\n" + body


# --------------------------------------------------------------------------
# Bundle documents
# --------------------------------------------------------------------------


@dataclass
class Doc:
    path: Path
    rel: str
    kind: str  # "root-index" | "index" | "log" | "concept"
    meta: dict[str, object] | None
    body: str
    body_line: int
    error: str | None = None

    @property
    def title(self) -> str:
        title = self.meta.get("title") if self.meta else None
        if isinstance(title, str) and title.strip():
            return title.strip()
        for _, level, text in headings(self):
            if level == 1:
                return text
        return self.path.stem

    def field(self, name: str) -> str:
        value = self.meta.get(name) if self.meta else None
        return "" if value is None else str(value)

    def tags(self) -> list[str]:
        value = self.meta.get("tags") if self.meta else None
        return [str(tag) for tag in value] if isinstance(value, list) else []


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def _doc_kind(rel: str) -> str:
    name = rel.rsplit("/", 1)[-1]
    if rel == "index.md":
        return "root-index"
    if name == "index.md":
        return "index"
    if name == "log.md":
        return "log"
    return "concept"


def load_doc(bundle: Path, path: Path) -> Doc:
    text = read_text(path)
    rel = path.relative_to(bundle).as_posix()
    kind = _doc_kind(rel)
    try:
        fm, body, body_line = split_frontmatter(text)
        meta = parse_frontmatter(fm) if fm is not None else None
    except FrontmatterError as error:
        return Doc(path, rel, kind, None, text, 1, str(error))
    return Doc(path, rel, kind, meta, body, body_line)


def _is_hidden(path: Path, bundle: Path) -> bool:
    return any(part.startswith(".") for part in path.relative_to(bundle).parts)


def iter_docs(bundle: Path) -> list[Doc]:
    paths = sorted(p for p in bundle.rglob("*.md") if p.is_file() and not _is_hidden(p, bundle))
    return [load_doc(bundle, path) for path in paths]


def _lines_outside_fences(doc: Doc) -> Iterator[tuple[int, str]]:
    fence: str | None = None
    for offset, line in enumerate(doc.body.split("\n")):
        stripped = line.lstrip()
        if stripped.startswith(("```", "~~~")):
            marker = stripped[:3]
            if fence is None:
                fence = marker
            elif marker == fence:
                fence = None
            continue
        if fence is None:
            yield doc.body_line + offset, line


def headings(doc: Doc) -> list[tuple[int, int, str]]:
    """(file line, level, text) for each Markdown heading outside code fences."""
    found: list[tuple[int, int, str]] = []
    for lineno, line in _lines_outside_fences(doc):
        match = _HEADING_RE.match(line)
        if match:
            found.append((lineno, len(match.group(1)), match.group(2)))
    return found


def find_bundle(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    env = os.environ.get("OKF_BUNDLE")
    if env:
        return Path(env).resolve()
    return Path(__file__).resolve().parents[2] / "knowledge"


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def bundle_path(bundle: Path, raw: str) -> Path:
    """Resolve a doc path given relative to cwd, the repo, or the bundle."""
    path = Path(raw)
    if not path.is_absolute():
        candidates = ((Path.cwd() / path).resolve(), (bundle.parent / path).resolve())
        path = next((c for c in candidates if _is_within(c, bundle)), bundle / path)
    path = path.resolve()
    if not _is_within(path, bundle):
        raise OkfError(f"path is outside the OKF bundle {bundle}: {raw}")
    return path


def resolve_spec_path(bundle: Path, doc_path: Path, value: str) -> Path | None:
    """Resolve an OKF link or path field (SPEC §6.1, §6.2).

    URLs return None, ``/x`` is bundle-relative, anything else is relative to the document.
    """
    if _SCHEME_RE.match(value):
        return None
    if value.startswith("/"):
        return Path(os.path.normpath(bundle / value.lstrip("/")))
    return Path(os.path.normpath(doc_path.parent / value))


def existing_doc(bundle: Path, raw: str) -> Doc:
    path = bundle_path(bundle, raw)
    if not path.is_file():
        raise OkfError(f"no such OKF document: {raw}")
    return load_doc(bundle, path)


def require_bundle(bundle: Path) -> None:
    if not (bundle / "index.md").is_file():
        raise OkfError(f"no OKF bundle at {bundle} (run `okf.py init`)")


def display(bundle: Path, rel: str) -> str:
    return f"{bundle.name}/{rel}" if rel else bundle.name


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def default_actor() -> str:
    return os.environ.get("OKF_ACTOR", "agent/unknown")


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


# --------------------------------------------------------------------------
# Index generation
# --------------------------------------------------------------------------


def _index_block(bundle: Path, directory: Path) -> str:
    entries: list[str] = []
    children = sorted(p for p in directory.iterdir() if not p.name.startswith("."))
    for child in children:
        if child.is_dir():
            entries.append(f"- [{child.name}/]({child.name}/index.md)")
    has_log = False
    for child in children:
        if not child.is_file() or child.suffix != ".md" or child.name == "index.md":
            continue
        if child.name == "log.md":
            has_log = True
            continue
        doc = load_doc(bundle, child)
        title = doc.title.replace("[", "\\[").replace("]", "\\]")
        entry = f"- [{title}]({child.name})"
        description = doc.field("description").strip()
        if description:
            entry += f" — {description}"
        entries.append(entry)
    if has_log:
        entries.append("- [Change log](log.md)")
    if not entries:
        entries.append("No documents yet.")
    return "\n".join([INDEX_BEGIN, *entries, INDEX_END])


def _render_index(existing: str | None, block: str, directory: Path, is_root: bool) -> str:
    if existing is None:
        if is_root:
            header = f'---\nokf_version: "{OKF_VERSION}"\n---\n\n# Knowledge Bundle\n\n'
        else:
            header = f"# {directory.name.replace('-', ' ').replace('_', ' ').title()}\n\n"
        return header + block + "\n"
    start = existing.find(INDEX_BEGIN)
    end = existing.find(INDEX_END)
    if start != -1 and end > start:
        return existing[:start] + block + existing[end + len(INDEX_END) :]
    return existing.rstrip("\n") + "\n\n" + block + "\n"


def reindex(bundle: Path, *, check: bool = False) -> list[Path]:
    """Regenerate the marked listing block in every index.md; return changed files."""
    directories = [bundle] + sorted(
        p for p in bundle.rglob("*") if p.is_dir() and not _is_hidden(p, bundle)
    )
    changed: list[Path] = []
    for directory in directories:
        index = directory / "index.md"
        existing = read_text(index) if index.is_file() else None
        rendered = _render_index(existing, _index_block(bundle, directory), directory, directory == bundle)
        if rendered != existing:
            changed.append(index)
            if not check:
                write_text(index, rendered)
    return changed


# --------------------------------------------------------------------------
# Change log
# --------------------------------------------------------------------------


def append_log(bundle: Path, message: str, day: date) -> Path:
    """Add a bullet under ``## day`` in log.md, keeping headings newest-first."""
    path = bundle / "log.md"
    text = read_text(path) if path.is_file() else LOG_TITLE + "\n"
    lines = text.rstrip("\n").split("\n")
    heading = f"## {day.isoformat()}"
    bullet = message if message.startswith("- ") else f"- {message}"

    existing = next((i for i, line in enumerate(lines) if line.strip() == heading), None)
    if existing is not None:
        end = next(
            (j for j in range(existing + 1, len(lines)) if lines[j].startswith("## ")),
            len(lines),
        )
        while end > existing + 1 and not lines[end - 1].strip():
            end -= 1
        lines.insert(end, bullet)
    else:
        position = len(lines)
        for i, line in enumerate(lines):
            text_after = line[3:].strip() if line.startswith("## ") else ""
            if _DATE_RE.match(text_after) and date.fromisoformat(text_after) < day:
                position = i
                break
        if position == len(lines):
            lines.extend(["", heading, "", bullet])
        else:
            lines[position:position] = [heading, "", bullet, ""]
    write_text(path, "\n".join(lines) + "\n")
    return path


# --------------------------------------------------------------------------
# Validation
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Issue:
    level: str  # "error" | "warning"
    path: str
    line: int | None
    message: str

    def render(self) -> str:
        location = f"{self.path}:{self.line}" if self.line else self.path
        return f"{self.level.upper()} {location}: {self.message}"


def _check_links(doc: Doc, bundle: Path, where: str) -> Iterator[Issue]:
    for lineno, line in _lines_outside_fences(doc):
        for target in _LINK_RE.findall(re.sub(r"`[^`]*`", "", line)):
            if target.startswith("#"):
                continue
            relative = unquote(target.split("#", 1)[0].split("?", 1)[0])
            if not relative:
                continue
            resolved = resolve_spec_path(bundle, doc.path, relative)
            if resolved is not None and not resolved.exists():
                yield Issue("error", where, lineno, f"broken link: {target}")


def _check_log(doc: Doc, where: str) -> Iterator[Issue]:
    previous: date | None = None
    seen: set[date] = set()
    for lineno, line in _lines_outside_fences(doc):
        if not line.startswith("## "):
            continue
        text = line[3:].strip()
        try:
            day = date.fromisoformat(text) if _DATE_RE.match(text) else None
        except ValueError:
            day = None
        if day is None:
            yield Issue("error", where, lineno, f"log heading must be '## YYYY-MM-DD', got {text!r}")
            continue
        if day in seen:
            yield Issue("error", where, lineno, f"duplicate log date {text}")
        elif previous is not None and day > previous:
            yield Issue("error", where, lineno, f"log dates must be newest-first ({text} after {previous})")
        seen.add(day)
        previous = day


def _check_concept(doc: Doc, bundle: Path, where: str) -> Iterator[Issue]:
    meta = doc.meta
    if meta is None:
        yield Issue("error", where, None, "concept document has no YAML frontmatter")
        return
    doc_type = meta.get("type")
    if not isinstance(doc_type, str) or not doc_type.strip():
        yield Issue("error", where, None, "frontmatter requires a non-empty 'type'")
    missing = [name for name in RECOMMENDED_FIELDS if meta.get(name) in (None, "", [])]
    if missing:
        yield Issue("warning", where, None, f"missing recommended fields: {', '.join(missing)}")
    status = meta.get("status")
    if status is not None and status not in STATUS_VALUES:
        yield Issue("warning", where, None, f"status {status!r} not in {sorted(STATUS_VALUES)}")
    evidence = meta.get("evidence")
    if evidence is not None and evidence not in EVIDENCE_VALUES:
        yield Issue("warning", where, None, f"evidence {evidence!r} not in {sorted(EVIDENCE_VALUES)}")
    tags = meta.get("tags")
    if tags is not None and not isinstance(tags, list):
        yield Issue("warning", where, None, "'tags' should be a list")
    generated = meta.get("generated")
    if generated is not None and not (isinstance(generated, dict) and generated.get("by")):
        yield Issue("warning", where, None, "'generated' should be a map with 'by' (and 'at')")
    verified = meta.get("verified")
    events = [] if verified is None else verified if isinstance(verified, list) else [verified]
    if not all(isinstance(event, dict) and event.get("by") and event.get("at") for event in events):
        yield Issue("warning", where, None, "'verified' should be a {by, at} map or a list of them")
    actors = {event.get("by") for event in events if isinstance(event, dict)}
    if evidence == "validated-runtime" and not actors - {None, SCAN_ONLY_ACTOR}:
        yield Issue(
            "warning",
            where,
            None,
            "evidence 'validated-runtime' needs verification by an executed test, not a repository scan",
        )
    sources = meta.get("sources")
    if sources is None:
        return
    if not isinstance(sources, list):
        yield Issue("warning", where, None, "'sources' should be a list")
        return
    for source in sources:
        resource = source.get("resource") if isinstance(source, dict) else None
        if not isinstance(resource, str) or not resource.strip():
            yield Issue("warning", where, None, "each 'sources' entry needs a 'resource'")
            continue
        resolved = resolve_spec_path(bundle, doc.path, resource)
        if resolved is not None and not resolved.exists():
            yield Issue("warning", where, None, f"source resource not found: {resource}")


def validate(bundle: Path) -> list[Issue]:
    if not bundle.is_dir():
        return [Issue("error", display(bundle, ""), None, "bundle directory does not exist")]
    issues: list[Issue] = []
    if not (bundle / "index.md").is_file():
        issues.append(Issue("error", display(bundle, "index.md"), None, "bundle root index.md is missing"))
    for doc in iter_docs(bundle):
        where = display(bundle, doc.rel)
        if doc.error:
            issues.append(Issue("error", where, None, doc.error))
            continue
        if doc.kind == "root-index":
            extra = sorted(set(doc.meta or {}) - {"okf_version"})
            if extra:
                issues.append(Issue("error", where, None, f"root index frontmatter may only contain okf_version (found {', '.join(extra)})"))
        elif doc.kind in ("index", "log") and doc.meta is not None:
            issues.append(Issue("error", where, None, f"{doc.path.name} must not have frontmatter"))
        if doc.kind == "log":
            issues.extend(_check_log(doc, where))
        elif doc.kind == "concept":
            issues.extend(_check_concept(doc, bundle, where))
        issues.extend(_check_links(doc, bundle, where))
    for index in reindex(bundle, check=True):
        rel = index.relative_to(bundle).as_posix()
        issues.append(Issue("warning", display(bundle, rel), None, "index is out of date (run `okf.py reindex`)"))
    return issues


# --------------------------------------------------------------------------
# Change impact
# --------------------------------------------------------------------------


def _git_changed(repo: Path, rev: str) -> list[str]:
    commands = (
        ["git", "-C", str(repo), "diff", "--name-only", "--relative", rev, "--"],
        ["git", "-C", str(repo), "ls-files", "--others", "--exclude-standard"],
    )
    paths: list[str] = []
    for command in commands:
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            raise OkfError(f"{' '.join(command[3:5])} failed: {result.stderr.strip()}")
        paths.extend(line for line in result.stdout.splitlines() if line.strip())
    return paths


def _repo_relative(repo: Path, raw: str) -> str:
    path = Path(raw)
    if not path.is_absolute():
        return _normalize_resource(path.as_posix())
    try:
        return path.resolve().relative_to(repo).as_posix()
    except ValueError:
        return path.as_posix()


def _normalize_resource(resource: str) -> str:
    while resource.startswith("./"):
        resource = resource[2:]
    return resource.rstrip("/")


def affected(bundle: Path, changed: list[str]) -> tuple[dict[str, list[str]], list[str]]:
    """Map changed repo paths to concept docs whose ``sources`` cover them."""
    bundle_prefix = bundle.name + "/"
    changed = sorted({path for path in changed if not path.startswith(bundle_prefix)})
    repo = bundle.parent
    hits: dict[str, list[str]] = {}
    covered: set[str] = set()
    for doc in iter_docs(bundle):
        if doc.kind != "concept" or not doc.meta or not isinstance(doc.meta.get("sources"), list):
            continue
        resources: list[str] = []
        for source in doc.meta["sources"]:
            raw = source.get("resource") if isinstance(source, dict) else None
            resolved = resolve_spec_path(bundle, doc.path, raw) if isinstance(raw, str) else None
            if resolved is not None and resolved != repo and _is_within(resolved, repo):
                resources.append(resolved.relative_to(repo).as_posix())
        matched = [
            path
            for path in changed
            if any(path == res or path.startswith(res + "/") for res in resources)
        ]
        if matched:
            hits[display(bundle, doc.rel)] = matched
            covered.update(matched)
    return hits, [path for path in changed if path not in covered]


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------


def _print_written(bundle: Path, paths: list[Path], verb: str = "updated") -> None:
    for path in paths:
        print(f"{verb} {display(bundle, path.relative_to(bundle).as_posix())}")


def cmd_init(bundle: Path, args: argparse.Namespace) -> int:
    bundle.mkdir(parents=True, exist_ok=True)
    for name in STANDARD_SECTIONS:
        (bundle / name).mkdir(exist_ok=True)
    if not (bundle / "log.md").exists():
        append_log(bundle, "Initialized OKF bundle (`okf.py init`).", date.today())
        print(f"created {display(bundle, 'log.md')}")
    _print_written(bundle, reindex(bundle))
    return 0


def cmd_list(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    docs = [doc for doc in iter_docs(bundle) if doc.kind == "concept"]
    if args.type:
        docs = [d for d in docs if d.field("type").lower() == args.type.lower()]
    if args.status:
        docs = [d for d in docs if d.field("status") == args.status]
    if args.tag:
        docs = [d for d in docs if args.tag in d.tags()]
    if args.under:
        prefix = bundle_path(bundle, args.under).relative_to(bundle).as_posix().rstrip("/") + "/"
        docs = [d for d in docs if prefix == "./" or d.rel.startswith(prefix)]
    rows = [
        {
            "path": display(bundle, d.rel),
            "type": d.field("type"),
            "status": d.field("status"),
            "title": d.title,
            "description": d.field("description"),
            "tags": d.tags(),
        }
        for d in docs
    ]
    if args.json:
        print(json.dumps(rows, indent=2, ensure_ascii=False))
        return 0
    if not rows:
        print("no matching documents", file=sys.stderr)
        return 0
    widths = {key: max(len(row[key]) for row in rows) for key in ("path", "type", "status")}
    for row in rows:
        print(
            f"{row['path']:<{widths['path']}}  {row['type'] or '-':<{widths['type']}}  "
            f"{row['status'] or '-':<{widths['status']}}  {row['title']}"
        )
        if args.long and row["description"]:
            print(f"    {row['description']}")
    return 0


def _find_section(doc: Doc, name: str) -> str:
    found = headings(doc)
    wanted = name.strip().lower()
    match = next((h for h in found if h[2].lower() == wanted), None)
    match = match or next((h for h in found if wanted in h[2].lower()), None)
    if match is None:
        available = ", ".join(h[2] for h in found) or "none"
        raise OkfError(f"section {name!r} not found in {doc.rel} (headings: {available})")
    lineno, level, _ = match
    end = next(
        (h[0] for h in found if h[0] > lineno and h[1] <= level),
        doc.body_line + len(doc.body.split("\n")),
    )
    lines = doc.body.split("\n")[lineno - doc.body_line : end - doc.body_line]
    return "\n".join(lines).rstrip()


def cmd_show(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    doc = existing_doc(bundle, args.path)
    if doc.error:
        raise OkfError(f"{doc.rel}: {doc.error}")
    show_all = not (args.meta or args.outline or args.section or args.body)
    if args.json:
        payload: dict[str, object] = {"path": display(bundle, doc.rel), "kind": doc.kind}
        if args.meta or show_all:
            payload["meta"] = doc.meta
        if args.outline or show_all:
            payload["outline"] = [{"line": n, "level": lv, "text": t} for n, lv, t in headings(doc)]
        if args.section:
            payload["sections"] = {name: _find_section(doc, name) for name in args.section}
        if args.body:
            payload["body"] = doc.body
        print(json.dumps(payload, indent=2, ensure_ascii=False))
        return 0
    if args.meta or show_all:
        for key, value in (doc.meta or {}).items():
            print("\n".join(dump_entry(key, value)))
        if show_all:
            print()
    if args.outline or show_all:
        for lineno, level, text in headings(doc):
            print(f"{lineno:>5}  {'#' * level} {text}")
    for name in args.section or []:
        print(_find_section(doc, name))
        print()
    if args.body:
        print(doc.body.strip("\n"))
    return 0


def cmd_search(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    flags = 0 if args.case_sensitive else re.IGNORECASE
    pattern = re.compile(args.pattern if args.regex else re.escape(args.pattern), flags)
    total = 0
    for path in sorted(p for p in bundle.rglob("*.md") if not _is_hidden(p, bundle)):
        rel = display(bundle, path.relative_to(bundle).as_posix())
        for lineno, line in enumerate(read_text(path).split("\n"), start=1):
            if pattern.search(line):
                total += 1
                if total <= args.limit:
                    print(f"{rel}:{lineno}: {line.strip()[:160]}")
    if total > args.limit:
        print(f"... {total - args.limit} more matches (raise --limit)")
    return 0


def _parse_sources(bundle: Path, doc_path: Path, raw_sources: list[str]) -> list[dict[str, object]]:
    """Turn repo-relative ``--source`` values into doc-relative OKF paths (SPEC §6.2)."""
    sources: list[dict[str, object]] = []
    for raw in raw_sources:
        given, sep, title = raw.partition("=")
        given = given.strip()
        if not given:
            raise OkfError(f"empty --source value: {raw!r}")
        resource = given
        if not _SCHEME_RE.match(given) and not given.startswith("/"):
            resource = Path(os.path.relpath(bundle.parent / given, doc_path.parent)).as_posix()
        entry: dict[str, object] = {"id": _slug(given), "resource": resource}
        if sep and title.strip():
            entry["title"] = title.strip()
        sources.append(entry)
    return sources


def cmd_new(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    path = bundle_path(bundle, args.path)
    if path.suffix != ".md" or path.name in ("index.md", "log.md"):
        raise OkfError("concept documents must be .md files other than index.md/log.md")
    if path.exists():
        raise OkfError(f"already exists: {display(bundle, path.relative_to(bundle).as_posix())}")
    meta: dict[str, object] = {
        "type": args.type,
        "title": args.title,
        "description": args.description,
        "tags": [t.strip() for t in args.tags.split(",") if t.strip()] if args.tags else [],
        "status": args.status,
        "evidence": args.evidence,
        "generated": {"by": args.by, "at": utc_now()},
    }
    sources = _parse_sources(bundle, path, args.source or [])
    if sources:
        meta["sources"] = sources
    frontmatter = [line for key, value in meta.items() for line in dump_entry(key, value)]
    text = "---\n" + "\n".join(frontmatter) + f"\n---\n\n# {args.title}\n\n{args.description}\n"
    write_text(path, text)
    _print_written(bundle, [path], "created")
    _print_written(bundle, reindex(bundle))
    return 0


def _edit_frontmatter(bundle: Path, raw_path: str, updates: dict[str, object]) -> Path:
    doc = existing_doc(bundle, raw_path)
    if doc.kind != "concept":
        raise OkfError(f"{doc.rel}: only concept documents carry editable frontmatter")
    if doc.error:
        raise OkfError(f"{doc.rel}: {doc.error}")
    write_text(doc.path, update_frontmatter(read_text(doc.path), updates))
    return doc.path


def cmd_set(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    updates: dict[str, object] = {}
    for assignment in args.assignments:
        key, sep, value = assignment.partition("=")
        if not sep or not _FIELD_NAME_RE.match(key):
            raise OkfError(f"expected KEY=VALUE, got {assignment!r}")
        updates[key] = parse_scalar(value)
    _print_written(bundle, [_edit_frontmatter(bundle, args.path, updates)])
    _print_written(bundle, reindex(bundle))
    return 0


def cmd_stamp(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    at = args.at or utc_now()
    if not _TIMESTAMP_RE.match(at):
        raise OkfError(f"--at must be an ISO-8601 timestamp like 2026-07-27T00:00:00Z, got {at!r}")
    written = [_edit_frontmatter(bundle, raw, {args.field: {"by": args.by, "at": at}}) for raw in args.paths]
    _print_written(bundle, written)
    return 0


def cmd_log(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    try:
        day = date.fromisoformat(args.date) if args.date else date.today()
    except ValueError as error:
        raise OkfError(f"--date must be YYYY-MM-DD, got {args.date!r}") from error
    message = " ".join(args.message).strip()
    if not message:
        raise OkfError("log message is empty")
    _print_written(bundle, [append_log(bundle, message, day)])
    return 0


def cmd_reindex(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    changed = reindex(bundle, check=args.check)
    if args.check:
        _print_written(bundle, changed, "stale")
        return 1 if changed else 0
    _print_written(bundle, changed)
    return 0


def cmd_validate(bundle: Path, args: argparse.Namespace) -> int:
    issues = validate(bundle)
    errors = sum(issue.level == "error" for issue in issues)
    warnings = len(issues) - errors
    if args.json:
        print(json.dumps([issue.__dict__ for issue in issues], indent=2, ensure_ascii=False))
    else:
        for issue in issues:
            print(issue.render())
        print(f"{'FAIL' if errors or (args.strict and warnings) else 'OK'}: {errors} errors, {warnings} warnings")
    return 1 if errors or (args.strict and warnings) else 0


def cmd_affected(bundle: Path, args: argparse.Namespace) -> int:
    require_bundle(bundle)
    repo = bundle.parent
    changed = [_repo_relative(repo, raw) for raw in args.paths]
    if args.git is not None or not args.paths:
        changed += _git_changed(repo, args.git or "HEAD")
    hits, uncovered = affected(bundle, changed)
    if args.json:
        print(json.dumps({"affected": hits, "uncovered": uncovered}, indent=2))
        return 0
    if not hits and not uncovered:
        print("no changed files outside the bundle")
        return 0
    for doc_path, files in hits.items():
        print(doc_path)
        for path in files:
            print(f"  <- {path}")
    if uncovered:
        print("uncovered (no OKF doc lists these in sources):")
        for path in uncovered:
            print(f"  {path}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="okf.py",
        description="Token-cheap, deterministic operations on the OKF knowledge bundle.",
        epilog=(
            "Doc paths may be bundle-relative (capabilities/x.md) or repo-relative "
            "(knowledge/capabilities/x.md). Bundle: --bundle, $OKF_BUNDLE, or <repo>/knowledge."
        ),
    )
    parser.add_argument("--bundle", help="path to the OKF bundle directory")
    sub = parser.add_subparsers(dest="command", required=True, metavar="COMMAND")

    p = sub.add_parser("init", help="create the bundle skeleton (idempotent)")
    p.set_defaults(handler=cmd_init)

    p = sub.add_parser("list", help="one line per concept doc: path, type, status, title")
    p.add_argument("--type", help="filter by frontmatter type (case-insensitive)")
    p.add_argument("--status", help="filter by status")
    p.add_argument("--tag", help="filter by tag")
    p.add_argument("--under", help="only docs below this directory")
    p.add_argument("--long", action="store_true", help="also print descriptions")
    p.add_argument("--json", action="store_true")
    p.set_defaults(handler=cmd_list)

    p = sub.add_parser("show", help="print parts of a doc (default: frontmatter + outline)")
    p.add_argument("path")
    p.add_argument("--meta", action="store_true", help="frontmatter only")
    p.add_argument("--outline", action="store_true", help="headings with file line numbers")
    p.add_argument("--section", action="append", metavar="HEADING", help="one section's text (repeatable)")
    p.add_argument("--body", action="store_true", help="full body without frontmatter")
    p.add_argument("--json", action="store_true")
    p.set_defaults(handler=cmd_show)

    p = sub.add_parser("search", help="search all bundle files; prints path:line: text")
    p.add_argument("pattern")
    p.add_argument("--regex", action="store_true", help="treat pattern as a regular expression")
    p.add_argument("--case-sensitive", action="store_true")
    p.add_argument("--limit", type=int, default=50)
    p.set_defaults(handler=cmd_search)

    p = sub.add_parser("new", help="create a concept doc with complete frontmatter, then reindex")
    p.add_argument("path", help="e.g. capabilities/build.md")
    p.add_argument("--type", required=True)
    p.add_argument("--title", required=True)
    p.add_argument("--description", required=True)
    p.add_argument("--tags", help="comma-separated")
    p.add_argument("--status", default="draft", choices=sorted(STATUS_VALUES))
    p.add_argument("--evidence", default="project-documentation", choices=sorted(EVIDENCE_VALUES))
    p.add_argument(
        "--source",
        action="append",
        metavar="PATH[=TITLE]",
        help="repo-relative path or URL, stored doc-relative per OKF §6.2 (repeatable)",
    )
    p.add_argument("--by", default=default_actor(), help="generated.by actor (default: $OKF_ACTOR)")
    p.set_defaults(handler=cmd_new)

    p = sub.add_parser("set", help="set frontmatter fields in place (VALUE parsed as inline YAML)")
    p.add_argument("path")
    p.add_argument("assignments", nargs="+", metavar="KEY=VALUE")
    p.set_defaults(handler=cmd_set)

    p = sub.add_parser("stamp", help="set generated/verified to {by, at: now} on docs")
    p.add_argument("field", choices=("generated", "verified"))
    p.add_argument("paths", nargs="+")
    p.add_argument("--by", required=True, help="actor, e.g. 'process:make test' or 'agent/model'")
    p.add_argument("--at", help="timestamp (default: now, UTC)")
    p.set_defaults(handler=cmd_stamp)

    p = sub.add_parser("log", help="append an entry under today's date in log.md")
    p.add_argument("message", nargs="+")
    p.add_argument("--date", help="YYYY-MM-DD (default: today)")
    p.set_defaults(handler=cmd_log)

    p = sub.add_parser("reindex", help="regenerate index.md listings")
    p.add_argument("--check", action="store_true", help="exit 1 if any index is stale; write nothing")
    p.set_defaults(handler=cmd_reindex)

    p = sub.add_parser("validate", help="check OKF structure, frontmatter, links, evidence; exit 1 on errors")
    p.add_argument("--strict", action="store_true", help="treat warnings as failures")
    p.add_argument("--json", action="store_true")
    p.set_defaults(handler=cmd_validate)

    p = sub.add_parser("affected", help="list docs whose sources cover changed files")
    p.add_argument("paths", nargs="*", help="changed repo-relative paths (default: git changes vs HEAD)")
    p.add_argument("--git", nargs="?", const="HEAD", metavar="REV", help="also include git changes vs REV")
    p.add_argument("--json", action="store_true")
    p.set_defaults(handler=cmd_affected)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return args.handler(find_bundle(args.bundle), args)
    except OkfError as error:
        print(f"okf: error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
