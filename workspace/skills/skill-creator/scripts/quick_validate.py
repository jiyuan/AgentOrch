#!/usr/bin/env python3
"""Quick validation for a SKILL.md folder.

Checks the same shape AgentOS's Rust validator enforces (frontmatter
delimiters, required `name` + `description`, lowercase hyphen-case name,
folder name matches `name`) plus the optional-field rules from the
upstream Anthropic skill-creator spec (`license`, `allowed-tools`,
`metadata`, `compatibility`, length and character limits).

Adapted from https://github.com/anthropics/skills/blob/main/skills/skill-creator/scripts/quick_validate.py.

Usage:
    python3 scripts/quick_validate.py <skill_directory>
"""

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    yaml = None


ALLOWED_PROPERTIES = {
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
}


def _parse_frontmatter(text):
    """Return the YAML frontmatter dict, or raise ValueError."""
    if not text.startswith("---"):
        raise ValueError("No YAML frontmatter found")
    match = re.match(r"^---\n(.*?)\n---", text, re.DOTALL)
    if not match:
        raise ValueError("Invalid frontmatter format")
    frontmatter_text = match.group(1)
    if yaml is None:
        # Minimal fallback parser: scalar `key: value` lines only. Good
        # enough to verify the required fields without pulling in PyYAML.
        data = {}
        for line in frontmatter_text.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#") or ":" not in stripped:
                continue
            key, _, value = stripped.partition(":")
            data[key.strip()] = value.strip()
        return data
    try:
        data = yaml.safe_load(frontmatter_text)
    except yaml.YAMLError as exc:
        raise ValueError(f"Invalid YAML in frontmatter: {exc}") from exc
    if not isinstance(data, dict):
        raise ValueError("Frontmatter must be a YAML dictionary")
    return data


def validate_skill(skill_path):
    """Return (ok, message). `ok=True` means the skill passes validation."""
    skill_path = Path(skill_path)
    skill_md = skill_path / "SKILL.md"
    if not skill_md.exists():
        return False, f"SKILL.md not found in {skill_path}"

    content = skill_md.read_text()
    try:
        frontmatter = _parse_frontmatter(content)
    except ValueError as exc:
        return False, str(exc)

    unexpected = set(frontmatter.keys()) - ALLOWED_PROPERTIES
    if unexpected:
        return False, (
            f"Unexpected key(s) in SKILL.md frontmatter: "
            f"{', '.join(sorted(unexpected))}. "
            f"Allowed: {', '.join(sorted(ALLOWED_PROPERTIES))}"
        )

    if "name" not in frontmatter:
        return False, "Missing 'name' in frontmatter"
    if "description" not in frontmatter:
        return False, "Missing 'description' in frontmatter"

    name = frontmatter.get("name", "")
    if not isinstance(name, str):
        return False, f"Name must be a string, got {type(name).__name__}"
    name = name.strip()
    if not name:
        return False, "Name is empty"
    if not re.match(r"^[a-z0-9-]+$", name):
        return False, (
            f"Name '{name}' must be kebab-case "
            "(lowercase letters, digits, and hyphens only)"
        )
    if name.startswith("-") or name.endswith("-") or "--" in name:
        return False, (
            f"Name '{name}' cannot start/end with a hyphen "
            "or contain consecutive hyphens"
        )
    if len(name) > 64:
        return False, (
            f"Name is too long ({len(name)} chars); maximum 64"
        )

    if skill_path.name != name:
        return False, (
            f"Folder name '{skill_path.name}' does not match "
            f"frontmatter name '{name}'"
        )

    description = frontmatter.get("description", "")
    if not isinstance(description, str):
        return False, (
            f"Description must be a string, got {type(description).__name__}"
        )
    description = description.strip()
    if not description:
        return False, "Description is empty"
    if "<" in description or ">" in description:
        return False, "Description cannot contain angle brackets (< or >)"
    if len(description) > 1024:
        return False, (
            f"Description is too long ({len(description)} chars); maximum 1024"
        )

    compatibility = frontmatter.get("compatibility")
    if compatibility is not None:
        if not isinstance(compatibility, str):
            return False, (
                f"Compatibility must be a string, "
                f"got {type(compatibility).__name__}"
            )
        if len(compatibility) > 500:
            return False, (
                f"Compatibility is too long ({len(compatibility)} chars); "
                "maximum 500"
            )

    body = content[len(re.match(r"^---\n.*?\n---", content, re.DOTALL).group(0)):]
    if not body.strip():
        return False, "SKILL.md body is empty"

    return True, "Skill is valid!"


def main():
    if len(sys.argv) != 2:
        print("Usage: python3 scripts/quick_validate.py <skill_directory>")
        sys.exit(2)
    ok, message = validate_skill(sys.argv[1])
    print(message)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
