"""Behavioral tests for okf.py. Run: python3 -m unittest discover -s .ai/scripts/tests"""

from __future__ import annotations

import contextlib
import io
import shutil
import subprocess
import sys
import tempfile
import unittest
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import okf  # noqa: E402

try:
    import yaml
except ImportError:
    yaml = None

SPEC_EXAMPLE = """\
type: Capability
title: Short Descriptive Title
description: One sentence summary of the knowledge document.
tags: [tag1, tag2, tag3]
status: stable
evidence: implementation-and-current-specifications
generated: { by: "agent-name/model-id", at: 2026-07-27T00:00:00Z }
verified: { by: "process:repository-scan", at: 2026-07-27T00:00:00Z }
sources:
  - id: source-identifier
    resource: src/file.go # trailing comment
    title: Human-readable source description
"""


class FrontmatterTests(unittest.TestCase):
    def test_parses_agents_md_example(self) -> None:
        meta = okf.parse_frontmatter(SPEC_EXAMPLE)
        self.assertEqual(meta["tags"], ["tag1", "tag2", "tag3"])
        self.assertEqual(meta["generated"], {"by": "agent-name/model-id", "at": "2026-07-27T00:00:00Z"})
        self.assertEqual(
            meta["sources"],
            [{"id": "source-identifier", "resource": "src/file.go", "title": "Human-readable source description"}],
        )

    def test_rejects_list_continuation_at_dash_indent(self) -> None:
        with self.assertRaises(okf.FrontmatterError):
            okf.parse_frontmatter("sources:\n - id: x\n resource: y\n")

    def test_rejects_duplicate_keys_and_block_scalars(self) -> None:
        with self.assertRaises(okf.FrontmatterError):
            okf.parse_frontmatter("type: a\ntype: b\n")
        with self.assertRaises(okf.FrontmatterError):
            okf.parse_frontmatter("description: |\n  text\n")

    def test_dump_round_trips_and_is_real_yaml(self) -> None:
        meta = okf.parse_frontmatter(SPEC_EXAMPLE)
        meta["title"] = 'Kernel: build, "quoted" #1'
        meta["okf_version"] = "0.2"
        text = "\n".join(line for k, v in meta.items() for line in okf.dump_entry(k, v))
        self.assertEqual(okf.parse_frontmatter(text), meta)
        if yaml is not None:
            loaded = yaml.safe_load(text)
            self.assertEqual(loaded["title"], meta["title"])
            self.assertEqual(loaded["okf_version"], "0.2")
            self.assertEqual(loaded["sources"], meta["sources"])

    def test_update_preserves_untouched_lines(self) -> None:
        original = "---\n" + SPEC_EXAMPLE + "---\n\n# Body\n"
        updated = okf.update_frontmatter(original, {"status": "deprecated", "owner": "team-a"})
        self.assertIn("status: deprecated\n", updated)
        self.assertIn("resource: src/file.go # trailing comment\n", updated)
        self.assertTrue(updated.endswith("owner: team-a\n---\n\n# Body\n"))


class BundleTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(shutil.rmtree, self.repo)
        self.bundle = self.repo / "knowledge"
        self.run_ok("init")

    def run_cli(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = okf.main(["--bundle", str(self.bundle), *argv])
        return code, out.getvalue(), err.getvalue()

    def run_ok(self, *argv: str) -> str:
        code, out, err = self.run_cli(*argv)
        self.assertEqual(code, 0, f"{argv} failed: {out}{err}")
        return out

    def write(self, rel: str, text: str) -> Path:
        path = self.bundle / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def new_doc(self, rel: str = "capabilities/build.md", *extra: str) -> None:
        (self.repo / "scripts").mkdir(exist_ok=True)
        (self.repo / "scripts" / "build.sh").write_text("#!/bin/sh\n")
        self.run_ok(
            "new", rel, "--type", "Capability", "--title", "Build", "--description", "How builds run.",
            "--tags", "build,ci", "--source", "scripts/build.sh=Build script", "--by", "agent/test", *extra,
        )


class InitAndValidateTests(BundleTestCase):
    def test_init_produces_valid_bundle_and_is_idempotent(self) -> None:
        root = (self.bundle / "index.md").read_text()
        self.assertTrue(root.startswith('---\nokf_version: "0.2"\n---\n'))
        for section in okf.STANDARD_SECTIONS:
            self.assertTrue((self.bundle / section / "index.md").is_file())
        self.assertIn(f"## {date.today().isoformat()}", (self.bundle / "log.md").read_text())
        self.assertIn("OK: 0 errors", self.run_ok("validate"))
        self.assertEqual(self.run_ok("init"), "")

    def test_new_doc_validates_and_is_indexed(self) -> None:
        self.new_doc()
        self.assertIn("- [Build](build.md) — How builds run.", (self.bundle / "capabilities/index.md").read_text())
        self.assertIn("    resource: ../../scripts/build.sh\n", (self.bundle / "capabilities/build.md").read_text())
        out = self.run_ok("validate")
        self.assertIn("missing recommended fields: verified", out)
        self.run_ok("stamp", "verified", "capabilities/build.md", "--by", "process:make test")
        code, out, _ = self.run_cli("validate", "--strict")
        self.assertEqual(code, 0, out)

    def test_validate_reports_structural_errors(self) -> None:
        self.write("capabilities/index.md", "---\ntype: x\n---\n# Caps\n")
        self.write("concepts/no-type.md", "---\ntitle: T\n---\nSee [missing](nope.md) and `[code](x.md)`.\n")
        self.write("concepts/bad.md", "---\ntype: x\n")
        self.write("log.md", "# Log\n\n## 2026-01-01\n\n- a\n\n## 2026-02-01\n\n- b\n\n## Misc\n")
        code, out, _ = self.run_cli("validate")
        self.assertEqual(code, 1)
        self.assertIn("ERROR knowledge/capabilities/index.md: index.md must not have frontmatter", out)
        self.assertIn("ERROR knowledge/concepts/no-type.md: frontmatter requires a non-empty 'type'", out)
        self.assertIn("ERROR knowledge/concepts/no-type.md:4: broken link: nope.md", out)
        self.assertNotIn("x.md", out.replace("index.md", ""))
        self.assertIn("ERROR knowledge/concepts/bad.md: unterminated frontmatter", out)
        self.assertIn("log dates must be newest-first", out)
        self.assertIn("log heading must be '## YYYY-MM-DD'", out)

    def test_validated_runtime_requires_executed_verification(self) -> None:
        self.new_doc("capabilities/build.md", "--evidence", "validated-runtime")
        self.run_ok("stamp", "verified", "capabilities/build.md", "--by", okf.SCAN_ONLY_ACTOR)
        self.assertIn("needs verification by an executed test", self.run_ok("validate"))

    def test_spec_path_forms_and_verified_list(self) -> None:
        self.new_doc()
        self.write(
            "concepts/linked.md",
            "---\ntype: Concept\nevidence: validated-runtime\nverified:\n"
            '  - { by: "process:repository-scan", at: 2026-01-01T00:00:00Z }\n'
            '  - { by: "process:make test", at: 2026-01-02T00:00:00Z }\n'
            "sources:\n  - resource: /capabilities/build.md\n  - resource: https://example.com/x\n---\n"
            "See [build](/capabilities/build.md) and [gone](/capabilities/gone.md).\n",
        )
        code, out, _ = self.run_cli("validate")
        self.assertEqual(code, 1)
        self.assertIn("ERROR knowledge/concepts/linked.md:11: broken link: /capabilities/gone.md", out)
        self.assertNotIn("broken link: /capabilities/build.md", out)
        self.assertNotIn("source resource not found", out)
        self.assertNotIn("'verified' should be", out)
        self.assertNotIn("needs verification", out)


class ReadTests(BundleTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.write(
            "playbooks/deploy.md",
            "---\ntype: Playbook\ntitle: Deploy\nstatus: stable\ntags: [ops]\n---\n\n# Deploy\n\n"
            "## Prerequisites\n\nNeed keys.\n\n### Keys\n\nDetail.\n\n```sh\n# not a heading\n```\n\n"
            "## Steps\n\nRun it.\n",
        )

    def test_list_filters(self) -> None:
        self.new_doc()
        out = self.run_ok("list", "--type", "playbook")
        self.assertIn("knowledge/playbooks/deploy.md", out)
        self.assertNotIn("build.md", out)
        self.assertIn("build.md", self.run_ok("list", "--tag", "ci", "--under", "capabilities"))

    def test_show_outline_and_section(self) -> None:
        out = self.run_ok("show", "knowledge/playbooks/deploy.md", "--outline")
        self.assertIn("   10  ## Prerequisites", out)
        self.assertNotIn("not a heading", out)
        section = self.run_ok("show", "playbooks/deploy.md", "--section", "prereq")
        self.assertIn("### Keys", section)
        self.assertNotIn("Run it.", section)
        code, _, err = self.run_cli("show", "playbooks/deploy.md", "--section", "Nope")
        self.assertEqual(code, 1)
        self.assertIn("headings: Deploy, Prerequisites, Keys, Steps", err)

    def test_search_limit(self) -> None:
        out = self.run_ok("search", "deploy", "--limit", "1")
        self.assertEqual(len(out.splitlines()), 2)
        self.assertIn("more matches", out)


class WriteTests(BundleTestCase):
    def test_set_updates_field_and_reindexes(self) -> None:
        self.new_doc()
        self.run_ok("set", "capabilities/build.md", "status=stable", "title=Build System", "tags=[a, b]")
        doc = okf.load_doc(self.bundle, self.bundle / "capabilities/build.md")
        self.assertEqual((doc.meta["status"], doc.meta["tags"]), ("stable", ["a", "b"]))
        self.assertEqual(doc.meta["sources"][0]["resource"], "../../scripts/build.sh")
        self.assertIn("[Build System](build.md)", (self.bundle / "capabilities/index.md").read_text())

    def test_set_refuses_reserved_files(self) -> None:
        code, _, err = self.run_cli("set", "index.md", "type=x")
        self.assertEqual(code, 1)
        self.assertIn("only concept documents", err)

    def test_log_keeps_dates_newest_first(self) -> None:
        self.run_ok("log", "newest", "--date", "2099-01-02")
        self.run_ok("log", "older", "--date", "2099-01-01")
        self.run_ok("log", "second newest", "--date", "2099-01-02")
        text = (self.bundle / "log.md").read_text()
        self.assertLess(text.index("## 2099-01-02"), text.index("## 2099-01-01"))
        self.assertIn("## 2099-01-02\n\n- newest\n- second newest\n", text)
        self.assertIn("OK: 0 errors", self.run_ok("validate"))

    def test_reindex_preserves_prose_and_check_detects_staleness(self) -> None:
        index = self.bundle / "capabilities/index.md"
        index.write_text("# Capabilities\n\nHand-written intro.\n")
        code, out, _ = self.run_cli("reindex", "--check")
        self.assertEqual((code, out.strip()), (1, "stale knowledge/capabilities/index.md"))
        self.run_ok("reindex")
        text = index.read_text()
        self.assertTrue(text.startswith("# Capabilities\n\nHand-written intro.\n\n" + okf.INDEX_BEGIN))
        self.assertEqual(self.run_cli("reindex", "--check")[0], 0)


class AffectedTests(BundleTestCase):
    def test_maps_paths_to_docs_and_reports_uncovered(self) -> None:
        self.new_doc()
        self.write("concepts/dir.md", "---\ntype: Concept\nsources:\n  - resource: ../../src/\n---\n")
        out = self.run_ok(
            "affected", str(self.repo / "scripts/build.sh"), "./src/a/b.py", "other.txt", "knowledge/log.md"
        )
        self.assertIn("knowledge/capabilities/build.md\n  <- scripts/build.sh", out)
        self.assertIn("knowledge/concepts/dir.md\n  <- src/a/b.py", out)
        self.assertIn("uncovered (no OKF doc lists these in sources):\n  other.txt", out)
        self.assertNotIn("knowledge/log.md", out)

    @unittest.skipUnless(shutil.which("git"), "git not installed")
    def test_git_changes(self) -> None:
        self.new_doc()
        git = ["git", "-C", str(self.repo), "-c", "user.email=t@t", "-c", "user.name=t"]
        subprocess.run([*git, "init", "-q"], check=True)
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-qm", "init"], check=True)
        (self.repo / "scripts/build.sh").write_text("#!/bin/sh\necho changed\n")
        (self.repo / "new.txt").write_text("x")
        out = self.run_ok("affected")
        self.assertIn("knowledge/capabilities/build.md\n  <- scripts/build.sh", out)
        self.assertIn("  new.txt", out)


if __name__ == "__main__":
    unittest.main()
