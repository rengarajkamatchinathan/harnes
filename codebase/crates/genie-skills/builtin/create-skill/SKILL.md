---
name: create-skill
description: Create or improve a Genie skill (a SKILL.md folder). Use when the user wants to package a repeatable workflow, checklist or house style as a skill.
metadata:
  publisher: genie
---
# Create a skill

A skill is a folder with a `SKILL.md` file and optional `references/`, `assets/` and `scripts/` folders.

1. Ask what the skill should help with, when it should be used, and one or two example requests. Skip questions the user already answered.
2. Pick a `name`: lowercase letters, digits and hyphens, at most 64 characters. The folder has the same name.
3. Write the frontmatter:
   - `name`
   - `description`: one or two sentences saying what the skill does **and when to use it**. Genie shows only this text to the model until the skill is loaded, so make the trigger clear. No angle brackets.
   - optional `disable-model-invocation: true` if only the user should start it with `/name`.
4. Write the body as short, ordered instructions. Put long reference material in `references/*.md` and say in the body when to read each file.
5. Scripts go in `scripts/`. In chat they are listed but not run; in Code mode and the CLI they run through the shell with the user's normal permissions.
6. Show the user the finished files. To install, the user saves the folder under `~/.genie/skills/` or uploads it as a `.zip` in Settings → Skills.
