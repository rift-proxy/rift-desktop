"""Behavioral tests for the token-free workflow validator and reviewed residual gate."""

import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import install
import validate


POLICY = json.loads((validate.HERE / "residuals.json").read_text())


def finding(entry):
    return {
        "ident": entry["ident"],
        "determinations": {"severity": entry["severity"]},
        "locations": [{
            "symbolic": {
                "kind": "Primary",
                "key": {"Local": {"verbatim_path": entry["path"]}},
                "route": {"route": entry["route"]},
            },
            "concrete": {"feature": entry["feature"]},
        }],
    }


class ResidualPolicyTests(unittest.TestCase):
    def test_exact_known_site_is_reported_not_hidden(self):
        entry = next(entry for entry in POLICY if entry["ident"] == "superfluous-actions")
        accepted, rejected = validate.assess_findings([finding(entry)], POLICY, Path("."))
        self.assertEqual(len(accepted), 1)
        self.assertEqual(rejected, [])
        self.assertEqual(accepted[0]["reason"], entry["reason"])

    def test_unknown_rule_site_feature_severity_and_duplicate_fail_closed(self):
        entry = next(entry for entry in POLICY if entry["ident"] == "self-repository")
        original = finding(entry)
        variants = []
        unknown = copy.deepcopy(original)
        unknown["ident"] = "new-security-rule"
        variants.append(unknown)
        moved = copy.deepcopy(original)
        moved["locations"][0]["symbolic"]["route"]["route"][1] = {"Key": "new-job"}
        variants.append(moved)
        changed = copy.deepcopy(original)
        changed["locations"][0]["concrete"]["feature"] = "./.github/workflows/other.yml"
        variants.append(changed)
        severity = copy.deepcopy(original)
        severity["determinations"]["severity"] = "High"
        variants.append(severity)
        for variant in variants:
            with self.subTest(finding=variant):
                accepted, rejected = validate.assess_findings([variant], POLICY, Path("."))
                self.assertFalse(accepted)
                self.assertEqual(len(rejected), 1)
        accepted, rejected = validate.assess_findings([original, original], POLICY, Path("."))
        self.assertEqual(len(accepted), 1)
        self.assertEqual(len(rejected), 1)

    def test_privileged_residual_requires_shipped_workflow_and_helper_digests(self):
        entry = next(entry for entry in POLICY if entry["ident"] == "dangerous-triggers")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in (".github/workflows/pr-policy.yml", "tooling/pr-policy.mjs"):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes((validate.ROOT / path).read_bytes())
            accepted, rejected = validate.assess_findings([finding(entry)], POLICY, root)
            self.assertEqual(len(accepted), 1)
            self.assertFalse(rejected)
            for path in (".github/workflows/pr-policy.yml", "tooling/pr-policy.mjs"):
                with self.subTest(path=path):
                    file = root / path
                    original = file.read_bytes()
                    file.write_bytes(original + b"\n// unreviewed privileged change\n")
                    accepted, rejected = validate.assess_findings([finding(entry)], POLICY, root)
                    self.assertFalse(accepted)
                    self.assertEqual(len(rejected), 1)
                    file.write_bytes(original)

    def test_privileged_residual_cannot_drop_required_digest_pins(self):
        original = next(entry for entry in POLICY if entry["ident"] == "dangerous-triggers")
        variants = []
        missing = copy.deepcopy(original)
        missing.pop("reviewed_sha256")
        variants.append(missing)
        for pins in ({}, None, [], "not-a-digest-map"):
            entry = copy.deepcopy(original)
            entry["reviewed_sha256"] = pins
            variants.append(entry)
        for path in (".github/workflows/pr-policy.yml", "tooling/pr-policy.mjs"):
            missing = copy.deepcopy(original)
            missing["reviewed_sha256"].pop(path, None)
            variants.append(missing)
            for digest in ("", "not-a-sha256", None):
                invalid = copy.deepcopy(original)
                invalid["reviewed_sha256"][path] = digest
                variants.append(invalid)
        for entry in variants:
            with self.subTest(pins=entry.get("reviewed_sha256")):
                with self.assertRaisesRegex(ValueError, "requires reviewed SHA-256 pins"):
                    validate.assess_findings([finding(original)], [entry], validate.ROOT)

    def test_cli_gate_uses_shipped_pins_and_rejects_unreviewed_privileged_bytes(self):
        entry = next(entry for entry in POLICY if entry["ident"] == "dangerous-triggers")
        specs = json.loads((validate.HERE / "tools.json").read_text())["tools"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, tools, report = root / "source", root / "tools", root / "report"
            tools.mkdir()
            paths = (".github/workflows/pr-policy.yml", "tooling/pr-policy.mjs")
            for path in paths:
                file = source / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes((validate.ROOT / path).read_bytes())
            subprocess.run(["git", "init", "--quiet", str(source)], check=True)
            subprocess.run(["git", "-C", str(source), "add", "--", *paths], check=True)
            # External validators are deterministic fixtures; the real CLI, Git
            # tracking, source-file reads and shipped residual policy run unchanged.
            script = (
                f"#!{sys.executable}\n"
                "import json, pathlib, sys\n"
                "name = pathlib.Path(sys.argv[0]).name\n"
                f"versions = { {name: spec['version'] for name, spec in specs.items()}!r}\n"
                "if '--version' in sys.argv:\n"
                "    print(versions[name])\n"
                "elif name == 'zizmor':\n"
                f"    print(json.dumps({[finding(entry)]!r}))\n"
                "    sys.exit(14)\n"
            )
            for name in specs:
                executable = tools / name
                executable.write_text(script)
                executable.chmod(0o755)
            command = [
                sys.executable, str(Path(validate.__file__).resolve()),
                "--source", str(source), "--tools-bin", str(tools), "--report-dir", str(report),
            ]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            summary = json.loads((report / "summary.json").read_text())
            self.assertEqual(summary["gate"], "accepted-with-residuals")
            self.assertEqual(len(summary["accepted_residuals"]), 1)
            for path in paths:
                with self.subTest(path=path):
                    file = source / path
                    original = file.read_bytes()
                    file.write_bytes(original + b"\n// unreviewed privileged change\n")
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    summary = json.loads((report / "summary.json").read_text())
                    self.assertEqual(summary["gate"], "fail")
                    self.assertFalse(summary["accepted_residuals"])
                    self.assertEqual(len(summary["rejected_findings"]), 1)
                    file.write_bytes(original)

    def test_unknown_schema_does_not_pass(self):
        with self.assertRaises(ValueError):
            validate.assess_findings({}, POLICY, Path("."))
        with self.assertRaises(KeyError):
            validate.assess_findings([{"ident": "broken"}], POLICY, Path("."))


class IsolationTests(unittest.TestCase):
    def test_clean_environment_drops_tokens_and_configuration(self):
        with patch.dict(os.environ, {"GITHUB_TOKEN": "not-forwarded", "GH_TOKEN": "not-forwarded", "ACT_CONFIG": "not-forwarded"}):
            env = validate.clean_environment(Path("/isolated/home"), "/usr/bin:/bin")
        self.assertNotIn("GITHUB_TOKEN", env)
        self.assertNotIn("GH_TOKEN", env)
        self.assertNotIn("ACT_CONFIG", env)
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], os.devnull)

    def test_snapshot_scope_is_only_git_listed_regular_workflows(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflow = root / ".github/workflows/quality.yml"
            workflow.parent.mkdir(parents=True)
            workflow.write_text("name: Quality\n")
            result = subprocess.CompletedProcess([], 0, b".github/workflows/quality.yml\0")
            with patch.object(validate.subprocess, "run", return_value=result) as run:
                self.assertEqual(validate.tracked_workflows(root, {}), [Path(".github/workflows/quality.yml")])
                self.assertEqual(run.call_args.args[0][-2:], [".github/workflows/*.yml", ".github/workflows/*.yaml"])
                workflow.unlink()
                workflow.symlink_to(root / "ignored.env")
                with self.assertRaises(ValueError):
                    validate.tracked_workflows(root, {})

    def test_missing_tool_cannot_leave_a_stale_success_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory)
            (report / "summary.json").write_text('{"gate":"pass"}')
            with patch.object(validate, "tool_paths", side_effect=ValueError("missing pinned tool")):
                with self.assertRaisesRegex(ValueError, "missing pinned tool"):
                    validate.validate(Path("."), report)
            self.assertEqual(json.loads((report / "summary.json").read_text())["gate"], "fail")

    def test_bootstrap_rejects_bad_cached_checksum_without_extraction(self):
        specs = json.loads((install.HERE / "tools.json").read_text())["tools"]
        for name in ("actionlint", "zizmor", "pyflakes"):
            with self.subTest(tool=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                prefix = root / "prefix"
                downloads = prefix / "downloads"
                downloads.mkdir(parents=True)
                spec = specs[name]
                (downloads / spec["url"].rsplit("/", 1)[1]).write_bytes(b"untrusted")
                (root / "tools.json").write_text(json.dumps({"tools": {name: spec}}))
                with (
                    patch.object(install, "HERE", root),
                    patch.object(install.sys, "platform", "linux"),
                    patch.object(install.platform, "machine", return_value="x86_64"),
                    patch.object(os, "confstr", return_value="glibc 2.28"),
                    patch.object(install.tarfile, "open") as tar,
                    patch.object(install.zipfile, "ZipFile") as wheel,
                    patch.object(install.urllib.request, "urlopen") as network,
                ):
                    with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                        install.install(prefix)
                    tar.assert_not_called()
                    wheel.assert_not_called()
                    network.assert_not_called()
                self.assertEqual(list((prefix / "bin").iterdir()), [])
                self.assertFalse((prefix / "python-packages").exists())

    def test_bootstrap_rejects_unsupported_hosts_before_installing(self):
        hosts = [
            ("darwin", "x86_64", "glibc 2.28"),
            ("linux", "aarch64", "glibc 2.28"),
            ("linux", "x86_64", "glibc 2.9"),
            ("linux", "x86_64", "glibc 2.27"),
            ("linux", "x86_64", "musl 1.2.5"),
            ("linux", "x86_64", ""),
            ("linux", "x86_64", None),
            ("linux", "x86_64", "glibc unknown"),
        ]
        for system, machine, libc in hosts:
            with self.subTest(system=system, machine=machine, libc=libc), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "tools.json").write_text('{"tools": {}}')
                prefix = root / "not-installed"
                with (
                    patch.object(install, "HERE", root),
                    patch.object(install.sys, "platform", system),
                    patch.object(install.platform, "machine", return_value=machine),
                    patch.object(os, "confstr", return_value=libc),
                    patch.object(install, "checked_download") as download,
                ):
                    with self.assertRaisesRegex(ValueError, "glibc >= 2.28"):
                        install.install(prefix)
                    download.assert_not_called()
                self.assertFalse(prefix.exists())

    def test_bootstrap_rejects_unavailable_libc_detection(self):
        for error in (OSError("unavailable"), ValueError("unsupported confstr")):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "tools.json").write_text('{"tools": {}}')
                prefix = root / "not-installed"
                with (
                    patch.object(install, "HERE", root),
                    patch.object(install.sys, "platform", "linux"),
                    patch.object(install.platform, "machine", return_value="x86_64"),
                    patch.object(os, "confstr", side_effect=error),
                ):
                    with self.assertRaisesRegex(ValueError, "detected host glibc"):
                        install.install(prefix)
                self.assertFalse(prefix.exists())

    def test_bootstrap_accepts_glibc_baseline_and_newer_versions(self):
        for machine in ("x86_64", "AMD64"):
            for libc in ("glibc 2.28", "glibc 2.36", "glibc 2.100", "glibc 3.0"):
                with self.subTest(machine=machine, libc=libc), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    (root / "tools.json").write_text('{"tools": {}}')
                    prefix = root / "prefix"
                    with (
                        patch.object(install, "HERE", root),
                        patch.object(install.sys, "platform", "linux"),
                        patch.object(install.platform, "machine", return_value=machine),
                        patch.object(os, "confstr", return_value=libc),
                    ):
                        install.install(prefix)
                    self.assertTrue((prefix / "bin").is_dir())


if __name__ == "__main__":
    unittest.main()
