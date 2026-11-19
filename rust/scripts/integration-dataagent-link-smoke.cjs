// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const dataagent = require('./integration-dataagent-smoke.cjs');
const update = require('./integration-update-smoke.cjs');

const ENTRY = {
  driver: 'rust/scripts/integration-dataagent-link-smoke.cjs',
  candidate: 'dev/marcozo/integrate-dataagent-staging-datasource-l-tomff8',
  base: '313271f4104a562d853318503f8e14f552ac6dc2',
  source: '1785a96cb74c325c6ee94bcffad72f98a9882ac9',
  leaves: [...dataagent.LEAVES, 'v2_link_dataagent_host', 'v2_link_dataagent_process'],
  inputs: [...dataagent.INPUTS,
    'rust/clients/cli/app/src/v2_link_dataagent_host.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main_tests.rs',
    'rust/clients/cli/app/tests/v2_link_dataagent_host.rs',
    'rust/clients/cli/app/tests/v2_link_dataagent_process.rs',
    'rust/clients/cli/commands/mutations/tests/v2_link.rs',
    'rust/clients/cli/commands/mutations/tests/v2_link_wire.rs',
    'rust/engine/control-plane/resources/items/tests/links.rs',
  ],
  commands: [{
    args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
      '--features', 'headless', '--bin', 'fab-cli-v2', 'v2_link_dataagent_main::tests::'],
    required: update.registeredTests('rust/clients/cli/app/src/v2_link_dataagent_main_tests.rs')
      .map(name => `v2_link_dataagent_main::tests::${name}`),
    exhaustive: true,
  }],
};

module.exports = {ENTRY};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); dataagent.main(process.argv[2], ENTRY); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
