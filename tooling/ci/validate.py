#!/usr/bin/env python3
"""Validate Git-tracked public workflows, without tokens or workflow execution."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
EVENTS = ("pull_request", "workflow_dispatch", "push", "schedule", "pull_request_target", "workflow_call")


def clean_environment(home, path):
    return {
        "PATH": path,
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "LANG": "C.UTF-8",
        "NO_COLOR": "1",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "PYTHONDONTWRITEBYTECODE": "1",
    }


def tracked_workflows(source, env):
    result = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "ls-files", "-z", "--",
         ".github/workflows/*.yml", ".github/workflows/*.yaml"],
        cwd=source, env=env, check=True, capture_output=True,
    )
    paths = sorted(set(Path(os.fsdecode(item)) for item in result.stdout.split(b"\0") if item))
    if not paths:
        raise ValueError("No Git-tracked public workflows found")
    for path in paths:
        if path.parent != Path(".github/workflows") or path.is_absolute():
            raise ValueError(f"Unexpected tracked workflow path: {path}")
        if (source / path).is_symlink() or not (source / path).is_file():
            raise ValueError(f"Workflow must be a present regular tracked file: {path}")
    return paths


def assess_findings(findings, policy, source):
    """Match each result once by rule, severity, workflow, symbolic route and source feature."""
    if not isinstance(findings, list):
        raise ValueError("Unexpected zizmor JSON schema")
    for entry in policy:
        if entry["ident"] == "dangerous-triggers":
            reviewed = entry.get("reviewed_sha256")
            required = {entry["path"], ".github/workflows/pr-policy.yml", "tooling/pr-policy.mjs"}
            if (not isinstance(reviewed, dict) or not required.issubset(reviewed)
                    or any(not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)
                           for digest in reviewed.values())):
                raise ValueError("Privileged trigger residual requires reviewed SHA-256 pins for workflow and helper")
    accepted, rejected, used = [], [], set()
    for finding in findings:
        primary = [loc for loc in finding["locations"] if loc["symbolic"]["kind"] == "Primary"]
        if len(primary) != 1:
            raise ValueError("Expected exactly one primary zizmor location")
        loc = primary[0]
        identity = {
            "ident": finding["ident"],
            "severity": finding["determinations"]["severity"],
            "path": loc["symbolic"]["key"]["Local"]["verbatim_path"],
            "route": loc["symbolic"]["route"]["route"],
            "feature": loc["concrete"]["feature"],
        }
        match = next((i for i, entry in enumerate(policy)
                      if i not in used and all(entry[key] == value for key, value in identity.items())), None)
        if match is not None:
            entry = policy[match]
            for path, expected in entry.get("reviewed_sha256", {}).items():
                if hashlib.sha256((source / path).read_bytes()).hexdigest() != expected:
                    match = None
                    break
        if match is None:
            rejected.append(identity)
        else:
            used.add(match)
            accepted.append({**identity, "reason": policy[match]["reason"]})
    return accepted, rejected


def tool_paths(explicit):
    specs = json.loads((HERE / "tools.json").read_text())["tools"]
    candidates = [explicit] if explicit else [
        Path.home() / ".local/share/rift-ci-tools/public-ci-v1/bin",
        Path.home() / ".local/bin",
        # Reuse retained checksum-pinned audit installations; versions are checked below.
        *sorted((Path.home() / ".local/share/rift-ci-tools").glob("*/bin")),
    ]
    tools = {}
    for name in specs:
        found = next((directory / name for directory in candidates
                      if directory and (directory / name).is_file() and os.access(directory / name, os.X_OK)), None)
        if found is None and not explicit:
            executable = shutil.which(name)
            found = Path(executable) if executable else None
        if found is None:
            raise ValueError(f"Missing {name}; run python3 {HERE / 'install.py'}")
        tools[name] = str(found.resolve())
    return specs, tools


def validate(source, report, explicit=None):
    report.mkdir(parents=True, exist_ok=True)
    # Never leave an earlier successful summary behind if this invocation aborts.
    (report / "summary.json").write_text('{"gate":"fail","reason":"Validation has not completed"}\n')
    (report / "commands.json").write_text("[]\n")
    specs, tools = tool_paths(explicit)
    with tempfile.TemporaryDirectory(prefix="rift-public-ci-") as temporary:
        temporary = Path(temporary)
        home = temporary / "home"
        home.mkdir()
        snapshot = temporary / "project"
        snapshot.mkdir()
        path = os.pathsep.join(dict.fromkeys([str(Path(value).parent) for value in tools.values()]
                                            + ["/usr/local/bin", "/usr/bin", "/bin"]))
        env = clean_environment(home, path)
        paths = tracked_workflows(source, env)
        # Only YAML is copied. No source .env/.secrets/.actrc, ignored build or corpus bytes.
        for relative in paths:
            target = snapshot / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((source / relative).read_bytes())
        subprocess.run(["git", "init", "--quiet", str(snapshot)], env=env, check=True)
        empty_config = temporary / "empty.yaml"
        empty_config.write_text("{}\n")
        results = []

        def run(label, command):
            result = subprocess.run(command, cwd=snapshot, env=env, capture_output=True, text=True, timeout=120)
            (report / f"{label}.stdout").write_text(result.stdout)
            (report / f"{label}.stderr").write_text(result.stderr)
            results.append({"check": label, "command": command, "exit": result.returncode})
            (report / "commands.json").write_text(json.dumps(results, indent=2) + "\n")
            print(f"{label}: raw exit {result.returncode}")
            return result

        for name, executable in tools.items():
            version = run(f"version-{name}", [executable, "--version"])
            expected = re.escape(specs[name]["version"])
            if version.returncode or not re.search(r"(?<![\d.])" + expected + r"(?![\d.])", version.stdout + version.stderr):
                raise ValueError(f"Wrong {name} version; expected {specs[name]['version']}. Run install.py")

        workflow_args = [str(path) for path in paths]
        actionlint = run("actionlint", [tools["actionlint"], "-no-color", "-oneline", "-config-file", str(empty_config),
                                      "-shellcheck", tools["shellcheck"], "-pyflakes", tools["pyflakes"], *workflow_args])
        ghalint = run("ghalint", [tools["ghalint"], "--log-color", "never", "--config", str(empty_config), "run"])
        zizmor = run("zizmor", [tools["zizmor"], "--pedantic", "--offline", "--no-config", "--no-ignores",
                                "--strict-collection", "--no-progress", "--format", "json", *workflow_args])
        findings = json.loads(zizmor.stdout)
        policy = json.loads((HERE / "residuals.json").read_text())
        reviewed_paths = {path for entry in policy for path in entry.get("reviewed_sha256", {})}
        for relative in reviewed_paths:
            file = source / relative
            if file.is_symlink() or not file.is_file():
                raise ValueError(f"Reviewed privileged input is not a regular source file: {relative}")
            subprocess.run(
                ["git", "-c", "core.fsmonitor=false", "ls-files", "--error-unmatch", "--", relative],
                cwd=source, env=env, check=True, capture_output=True,
            )
        accepted, rejected = assess_findings(findings, policy, source)
        # zizmor v1 exit codes 10..14 encode finding severity; 1/2 and signals are tool failures.
        zizmor_ok = zizmor.returncode in (0, 10, 11, 12, 13, 14) and not rejected
        if (zizmor.returncode == 0) != (not findings):
            zizmor_ok = False
        act_args = [tools["act"], "--secret-file", os.devnull, "--env-file", os.devnull,
                    "--var-file", os.devnull, "--input-file", os.devnull,
                    "--container-daemon-socket", "-", "--no-cache-server", "--strict"]
        graphs_ok = True
        for event in EVENTS:
            graphs_ok &= run(f"act-list-{event}", [*act_args, event, "--list"]).returncode == 0
        for workflow in workflow_args:
            label = "act-graph-" + Path(workflow).stem
            graphs_ok &= run(label, [*act_args, "--detect-event", "-W", workflow, "--graph"]).returncode == 0
        gate_ok = actionlint.returncode == 0 and ghalint.returncode == 0 and zizmor_ok and graphs_ok
        summary = {
            "gate": "accepted-with-residuals" if gate_ok and accepted else "pass" if gate_ok else "fail",
            "zizmor_raw_exit": zizmor.returncode,
            "zizmor_clean": not findings and zizmor.returncode == 0,
            "workflows": workflow_args,
            "accepted_residuals": accepted,
            "rejected_findings": rejected,
            "commands": results,
            "limits": [
                "No workflow/action code executed; no tokens, original ignored files or private history read.",
                "Offline zizmor: token-dependent online audits are not exercised.",
                "actionlint uses real ShellCheck/pyflakes with upstream built-in ShellCheck exclusions; PowerShell is not shell-checked.",
                "act list/graph is static graph/schema evidence, not GitHub permission, environment approval or Windows execution evidence.",
                "Pinned action SHA existence/remote input metadata and hosted event allow policies are not checked offline.",
            ],
        }
        (report / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"Public CI gate: {summary['gate']}; zizmor raw exit {zizmor.returncode}; NOT CLEAN" if findings
              else f"Public CI gate: {summary['gate']}; no zizmor findings")
        for finding in accepted:
            print(f"ACCEPTED RESIDUAL [{finding['severity']}] {finding['ident']} {finding['path']}: {finding['reason']}")
        for finding in rejected:
            print(f"BLOCKED [{finding['severity']}] {finding['ident']} {finding['path']}")
        print(f"Reports: {report}")
        return 0 if gate_ok else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT, help="Public project Git root")
    parser.add_argument("--report-dir", type=Path, default=ROOT / "reports/public-ci")
    parser.add_argument("--tools-bin", type=Path, help="Reuse an explicit installed set of the pinned executables")
    args = parser.parse_args()
    try:
        return validate(args.source.resolve(), args.report_dir.resolve(), args.tools_bin.resolve() if args.tools_bin else None)
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        print(f"Public CI validation FAILED: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
