# Generated llms.txt For Project Output Handoff

Finite Sites synthesizes `/llms.txt` for editable Project Outputs when the
active Version did not publish that path itself.

The file is platform guidance for agents. It explains that the output is backed
by a Project Repository, names the Project slug and Git Remote, points to
`fsite describe workflow edit-shared-project --output json` and
`fsite auth git PROJECT [--email EMAIL] --store --output json`, and tells
agents to clone, edit source/deploy bytes, commit, and push the configured
Deploy Branch.

Generated handoff should also state that there is no direct upload command in
the current model. Agents publish by pushing git commits whose `finite.toml`
selects the served output path.

This must not imply that agents should omit source files. The Project
Repository is the whole shared source tree for authorized collaborators; the
output path only controls what viewers receive as website assets.

If a Project Output publishes its own `/llms.txt`, the authored file wins and
the platform does not synthesize one. Project-authored instructions are the
project authority.

Generated handoff files must not include private keys, tokens, email login
tokens, collaborator lists, viewer shares, or other permission metadata. They
may include public project/output identifiers and generic workflow commands.

Non-project outputs do not receive synthesized edit instructions. The supported
source-sharing model is the Project Repository.
