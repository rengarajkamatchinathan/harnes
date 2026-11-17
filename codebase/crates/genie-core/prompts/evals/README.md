# Prompt eval set (06 §2.6.8)

Scripted cases (identity, injection in web/file/MCP content, tool choice, artifacts vs inline,
memory save / don't-save, citation format, plan mode, denied-tool behaviour) run against each
provider family with the live smoke harness. Not built yet: the cases need the live layer
(03 §11 layer 4). Until then prompt changes are reviewed through the golden snapshots in
`crates/genie-core/tests/golden/prompts/`.
