#!/usr/bin/env python3
"""Package a skill folder into a distributable `.skill` archive.

A `.skill` file is a zip with the skill folder at its root. Build
artefacts, OS metadata, and the local `evals/` workspace are excluded so
the bundle stays small and reproducible.

Adapted from https://github.com/anthropics/skills/blob/main/skills/skill-creator/scripts/package_skill.py.

Usage:
    python3 scripts/package_skill.py <path/to/skill-folder> [output-directory]
"""

import fnmatch
import sys
import zipfile
from pathlib import Path

# Importing as a script also needs to work when this file is invoked
# directly (e.g. `python3 scripts/package_skill.py ...`). Add the
# scripts/ directory to sys.path before importing quick_validate.
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

from quick_validate import validate_skill  # noqa: E402


EXCLUDE_DIRS = {"__pycache__", "node_modules", ".git", ".venv"}
EXCLUDE_GLOBS = {"*.pyc", "*.pyo"}
EXCLUDE_FILES = {".DS_Store", "Thumbs.db"}
# Excluded only when they sit at the skill's root (not when nested).
ROOT_EXCLUDE_DIRS = {"evals"}


def should_exclude(rel_path):
    """Return True if `rel_path` (relative to the skill's parent) should be skipped."""
    parts = rel_path.parts
    if any(part in EXCLUDE_DIRS for part in parts):
        return True
    # parts[0] is the skill folder; parts[1] is its first-level child.
    if len(parts) > 1 and parts[1] in ROOT_EXCLUDE_DIRS:
        return True
    if rel_path.name in EXCLUDE_FILES:
        return True
    return any(fnmatch.fnmatch(rel_path.name, pat) for pat in EXCLUDE_GLOBS)


def package_skill(skill_path, output_dir=None):
    skill_path = Path(skill_path).resolve()

    if not skill_path.exists():
        print(f"Error: skill folder not found: {skill_path}")
        return None
    if not skill_path.is_dir():
        print(f"Error: path is not a directory: {skill_path}")
        return None
    if not (skill_path / "SKILL.md").exists():
        print(f"Error: SKILL.md not found in {skill_path}")
        return None

    print("Validating skill...")
    ok, message = validate_skill(skill_path)
    if not ok:
        print(f"Validation failed: {message}")
        print("Fix the validation errors before packaging.")
        return None
    print(f"OK: {message}\n")

    skill_name = skill_path.name
    output_path = (
        Path(output_dir).resolve() if output_dir else Path.cwd()
    )
    output_path.mkdir(parents=True, exist_ok=True)
    skill_filename = output_path / f"{skill_name}.skill"

    try:
        with zipfile.ZipFile(skill_filename, "w", zipfile.ZIP_DEFLATED) as zipf:
            for file_path in sorted(skill_path.rglob("*")):
                if not file_path.is_file():
                    continue
                arcname = file_path.relative_to(skill_path.parent)
                if should_exclude(arcname):
                    print(f"  Skipped: {arcname}")
                    continue
                zipf.write(file_path, arcname)
                print(f"  Added:   {arcname}")
        print(f"\nPackaged skill: {skill_filename}")
        return skill_filename
    except OSError as exc:
        print(f"Error writing .skill file: {exc}")
        if skill_filename.exists():
            skill_filename.unlink()
        return None


def main():
    if len(sys.argv) < 2:
        print(
            "Usage: python3 scripts/package_skill.py "
            "<path/to/skill-folder> [output-directory]"
        )
        sys.exit(2)

    skill_path = sys.argv[1]
    output_dir = sys.argv[2] if len(sys.argv) > 2 else None
    print(f"Packaging skill: {skill_path}")
    if output_dir:
        print(f"  Output directory: {output_dir}")
    print()
    result = package_skill(skill_path, output_dir)
    sys.exit(0 if result else 1)


if __name__ == "__main__":
    main()
