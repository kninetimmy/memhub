#!/usr/bin/env python3
"""Run a two-pass OpenAI code review over a git diff without manual copy/paste."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import textwrap
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


API_URL = "https://api.openai.com/v1/responses"
DEFAULT_PASS1_MODEL = "gpt-5.4-mini"
DEFAULT_PASS2_MODEL = "gpt-5.3-codex"
DEFAULT_PASS1_REASONING = "high"
DEFAULT_PASS2_REASONING = "xhigh"


@dataclass
class ReviewInput:
    source_label: str
    diff_stat: str
    diff_text: str
    git_status: str
    branch_name: str
    repo_root: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a two-pass OpenAI code review over a git diff.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    source_group = parser.add_mutually_exclusive_group()
    source_group.add_argument(
        "--diff-file",
        help="Read the review diff from a file instead of git.",
    )
    source_group.add_argument(
        "--staged",
        action="store_true",
        help="Review the staged diff.",
    )
    source_group.add_argument(
        "--working-tree",
        action="store_true",
        help="Review unstaged working tree changes.",
    )
    parser.add_argument(
        "--base",
        default="HEAD~1",
        help="Base revision for git diff review.",
    )
    parser.add_argument(
        "--head",
        default="HEAD",
        help="Head revision for git diff review.",
    )
    parser.add_argument(
        "--dot-mode",
        choices=("double", "triple"),
        default="triple",
        help="Use '..' or '...' between base and head when building the diff range.",
    )
    parser.add_argument(
        "--path",
        dest="paths",
        action="append",
        default=[],
        help="Limit the review to a path. Repeat to add more paths.",
    )
    parser.add_argument(
        "--output-dir",
        default="review_runs",
        help="Directory where prompts, outputs, and metadata will be written.",
    )
    parser.add_argument(
        "--pass1-model",
        default=DEFAULT_PASS1_MODEL,
        help="Model for the first review pass.",
    )
    parser.add_argument(
        "--pass2-model",
        default=DEFAULT_PASS2_MODEL,
        help="Model for the second review pass.",
    )
    parser.add_argument(
        "--pass1-reasoning",
        default=DEFAULT_PASS1_REASONING,
        help="reasoning.effort for the first review pass.",
    )
    parser.add_argument(
        "--pass2-reasoning",
        default=DEFAULT_PASS2_REASONING,
        help="reasoning.effort for the second review pass.",
    )
    parser.add_argument(
        "--max-chars",
        type=int,
        default=300_000,
        help="Fail if the diff exceeds this many characters.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=600,
        help="Per-request timeout for OpenAI API calls.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Write prompts and metadata only; do not call the API.",
    )
    return parser.parse_args()


def run_command(args: list[str]) -> str:
    result = subprocess.run(args, check=False, capture_output=True, text=True)
    if result.returncode != 0:
        stderr = result.stderr.strip() or "(no stderr)"
        raise RuntimeError(f"Command failed: {' '.join(args)}\n{stderr}")
    return result.stdout


def git_diff_args(parsed: argparse.Namespace, stat_only: bool) -> list[str]:
    args = ["git", "diff", "--no-ext-diff"]
    if stat_only:
        args.append("--stat")

    if parsed.staged:
        args.append("--cached")
    elif parsed.working_tree:
        pass
    else:
        dots = "..." if parsed.dot_mode == "triple" else ".."
        args.append(f"{parsed.base}{dots}{parsed.head}")

    if parsed.paths:
        args.append("--")
        args.extend(parsed.paths)
    return args


def load_review_input(parsed: argparse.Namespace) -> ReviewInput:
    repo_root = run_command(["git", "rev-parse", "--show-toplevel"]).strip()
    branch_name = run_command(["git", "rev-parse", "--abbrev-ref", "HEAD"]).strip()
    git_status = run_command(["git", "status", "--short"])

    if parsed.diff_file:
        diff_path = Path(parsed.diff_file)
        diff_text = diff_path.read_text(encoding="utf-8")
        diff_stat = "(not available for file input)\n"
        source_label = f"diff-file:{diff_path}"
    else:
        diff_text = run_command(git_diff_args(parsed, stat_only=False))
        diff_stat = run_command(git_diff_args(parsed, stat_only=True))
        if parsed.staged:
            source_label = "staged diff"
        elif parsed.working_tree:
            source_label = "working tree diff"
        else:
            dots = "..." if parsed.dot_mode == "triple" else ".."
            source_label = f"git diff {parsed.base}{dots}{parsed.head}"

    if not diff_text.strip():
        raise RuntimeError("The selected diff is empty. Pick a different range or change set.")
    if len(diff_text) > parsed.max_chars:
        raise RuntimeError(
            f"Diff size {len(diff_text)} exceeds --max-chars={parsed.max_chars}. Narrow the review scope."
        )

    return ReviewInput(
        source_label=source_label,
        diff_stat=diff_stat,
        diff_text=diff_text,
        git_status=git_status,
        branch_name=branch_name,
        repo_root=repo_root,
    )


def build_pass1_prompt(review_input: ReviewInput) -> str:
    return textwrap.dedent(
        f"""
        You are doing a deep first-pass code review focused on real engineering defects and maintainability risks.

        Review the provided diff and look for:
        - bugs and logic errors
        - edge case failures
        - unsafe assumptions
        - race conditions or concurrency issues
        - data corruption or persistence risks
        - API misuse
        - test gaps that hide real regressions
        - code that is technically working but improperly written in a way that is likely to cause future defects

        Do not praise the code. Do not summarize first. Start directly with findings.

        Output rules:
        - Only include findings that are concrete and worth an engineer's time.
        - Prefer fewer high-signal findings over many speculative ones.
        - For each finding, include:
          1. Severity: critical, high, medium, or low
          2. Title
          3. File and line or function reference
          4. Why it is a problem
          5. Realistic failure mode or regression risk
          6. Suggested fix direction
          7. Confidence: high, medium, or low
        - After findings, include:
          - Rejected concerns: things that looked suspicious but are probably fine
          - Coverage gaps: what you could not verify from the provided material
          - Final handoff summary for a second-pass reviewer:
            - Confirmed likely issues
            - Uncertain issues needing validation
            - Areas needing especially skeptical re-review
        - End with this exact wrapper so the second pass can validate it cleanly:

        FIRST_PASS_HANDOFF
        Likely issues:
        - [severity] [file/ref] [short title] [1-2 sentence reason]

        Uncertain issues:
        - [severity] [file/ref] [short title] [what needs validation]

        Rejected concerns:
        - [file/ref] [short reason]

        Coverage gaps:
        - [missing tests / missing files / missing runtime context]
        END_FIRST_PASS_HANDOFF

        Review context:
        - Repo root: {review_input.repo_root}
        - Branch: {review_input.branch_name}
        - Source: {review_input.source_label}

        Git status:
        {review_input.git_status or "(clean)"}

        Diff stat:
        {review_input.diff_stat}

        Diff to review:
        {review_input.diff_text}
        """
    ).strip() + "\n"


def build_pass2_prompt(review_input: ReviewInput, pass1_output: str) -> str:
    return textwrap.dedent(
        f"""
        You are the second-pass senior reviewer.

        Your job has two parts:
        1. Validate the first-pass review findings one by one.
        2. Perform your own independent deep code review of the same diff.

        You must be willing to disagree with the first pass. Do not assume its findings are correct.

        For each first-pass finding, label it as exactly one of:
        - Confirmed
        - Partially confirmed
        - Rejected
        - Needs more evidence

        For each validated finding, explain:
        - whether the first-pass reasoning is correct
        - what the real bug or risk is
        - whether the severity should change
        - the best fix direction

        Then perform an independent review and add any new findings not caught in pass 1.

        Output format:
        1. Validation of pass-1 findings
        2. New findings from independent review
        3. Final deduplicated review list ordered by severity
        4. Fix plan:
           - immediate fixes
           - tests to add
           - lower-priority cleanup
        5. Residual uncertainty:
           - what still needs runtime verification, tests, or broader codebase context

        Review standard:
        - prioritize bugs, regressions, unsafe behavior, and improperly written code with real engineering consequences
        - avoid cosmetic or style-only feedback
        - be explicit when a finding is weak or speculative
        - do not repeat duplicate findings

        Review context:
        - Repo root: {review_input.repo_root}
        - Branch: {review_input.branch_name}
        - Source: {review_input.source_label}

        Git status:
        {review_input.git_status or "(clean)"}

        Diff stat:
        {review_input.diff_stat}

        Code under review:
        {review_input.diff_text}

        First-pass review output to validate:
        {pass1_output}
        """
    ).strip() + "\n"


def extract_text(response_json: dict[str, Any]) -> str:
    output_text = response_json.get("output_text")
    if isinstance(output_text, str) and output_text.strip():
        return output_text

    chunks: list[str] = []
    for item in response_json.get("output", []):
        if not isinstance(item, dict):
            continue
        for content in item.get("content", []):
            if not isinstance(content, dict):
                continue
            text = content.get("text")
            if isinstance(text, str):
                chunks.append(text)
    text = "\n".join(chunk for chunk in chunks if chunk.strip()).strip()
    if text:
        return text
    raise RuntimeError("OpenAI response did not include any text output.")


def call_openai(model: str, reasoning: str, prompt: str, timeout_seconds: int) -> tuple[str, dict[str, Any]]:
    api_key = os.environ.get("OPENAI_API_KEY")
    if not api_key:
        raise RuntimeError("OPENAI_API_KEY is not set.")

    payload = {
        "model": model,
        "input": prompt,
        "reasoning": {"effort": reasoning},
    }
    request = urllib.request.Request(
        API_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            body = response.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        error_body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"OpenAI API request failed: HTTP {exc.code}\n{error_body}") from exc
    except urllib.error.URLError as exc:
        raise RuntimeError(f"OpenAI API request failed: {exc}") from exc

    response_json = json.loads(body)
    return extract_text(response_json), response_json


def write_text(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")


def main() -> int:
    parsed = parse_args()
    review_input = load_review_input(parsed)

    run_dir = Path(parsed.output_dir) / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir.mkdir(parents=True, exist_ok=False)

    metadata = {
        "created_at_utc": datetime.now(timezone.utc).isoformat(),
        "source": review_input.source_label,
        "repo_root": review_input.repo_root,
        "branch": review_input.branch_name,
        "pass1_model": parsed.pass1_model,
        "pass2_model": parsed.pass2_model,
        "pass1_reasoning": parsed.pass1_reasoning,
        "pass2_reasoning": parsed.pass2_reasoning,
        "dry_run": parsed.dry_run,
    }
    write_text(run_dir / "metadata.json", json.dumps(metadata, indent=2) + "\n")
    write_text(run_dir / "git_status.txt", review_input.git_status)
    write_text(run_dir / "diff_stat.txt", review_input.diff_stat)
    write_text(run_dir / "input.diff", review_input.diff_text)

    pass1_prompt = build_pass1_prompt(review_input)
    write_text(run_dir / "pass1_prompt.txt", pass1_prompt)

    if parsed.dry_run:
        print(f"Dry run complete. Prompt files written to {run_dir}")
        return 0

    print(f"Running pass 1 with {parsed.pass1_model}...")
    pass1_text, pass1_raw = call_openai(
        model=parsed.pass1_model,
        reasoning=parsed.pass1_reasoning,
        prompt=pass1_prompt,
        timeout_seconds=parsed.timeout_seconds,
    )
    write_text(run_dir / "pass1.md", pass1_text)
    write_text(run_dir / "pass1_raw.json", json.dumps(pass1_raw, indent=2) + "\n")

    pass2_prompt = build_pass2_prompt(review_input, pass1_text)
    write_text(run_dir / "pass2_prompt.txt", pass2_prompt)

    print(f"Running pass 2 with {parsed.pass2_model}...")
    pass2_text, pass2_raw = call_openai(
        model=parsed.pass2_model,
        reasoning=parsed.pass2_reasoning,
        prompt=pass2_prompt,
        timeout_seconds=parsed.timeout_seconds,
    )
    write_text(run_dir / "pass2.md", pass2_text)
    write_text(run_dir / "pass2_raw.json", json.dumps(pass2_raw, indent=2) + "\n")

    print("Two-pass review complete.")
    print(f"Run directory: {run_dir}")
    print(f"Pass 1: {run_dir / 'pass1.md'}")
    print(f"Pass 2: {run_dir / 'pass2.md'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:  # pragma: no cover - CLI-level failure handling
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
