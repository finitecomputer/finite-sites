# Generated llms.txt For Project Output Handoff

Finite Sites synthesizes `/llms.txt` for editable Project Outputs when the
active Version did not publish that path itself.

The file is platform guidance for agents. It explains that the output is backed
by a Project Repository, names the Project slug and Git Remote, points to
`fsite auth git PROJECT --email EMAIL --output json`, and tells agents to clone,
edit source/deploy bytes, commit, and push the configured Deploy Branch.

If a Project Output publishes its own `/llms.txt`, the authored file wins and
the platform does not synthesize one. Project-authored instructions are the
project authority.

Generated handoff files must not include private keys, tokens, email login
tokens, collaborator lists, viewer shares, or other permission metadata. They
may include public project/output identifiers and generic workflow commands.

Non-project outputs do not receive synthesized edit instructions. The supported
source-sharing model is the Project Repository.
