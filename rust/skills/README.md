<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI workflow skills

This directory is the documentation location for workflows that compose
`fab-rs` commands. No executable skill bundles are installed from this directory.

Skills invoke documented CLI operations and interpret their JSON output, exit
codes and partial-effect evidence. They do not duplicate SDK service logic,
acquire credentials independently, embed secrets or require an agent runtime to
be installed with the CLI. Applications can use the SDK independently of skills.

A workflow must declare its required commands, host features and input
capabilities. It must handle authorization failures, cancellation and accepted
operations without treating them as successful completion.

Skills are not executable plugins. This directory does not enable external
command discovery or a runtime loader. See the
[extension boundaries](../docs/extensions-and-skills.md) for the applicable
design constraints.
