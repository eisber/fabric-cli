// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const dataagent = require('./integration-dataagent-smoke.cjs');
const dataagentLink = require('./integration-dataagent-link-smoke.cjs');
const update = require('./integration-update-smoke.cjs');

const KQL_TARGET = 'v2_link_kql_host';
const CONTRACT_TARGETS = [
  {package: 'fabric-cli-mutations', target: 'v2_link',
    source: 'rust/clients/cli/commands/mutations/tests/v2_link.rs'},
  {package: 'fabric-cli-mutations', target: 'v2_link_wire',
    source: 'rust/clients/cli/commands/mutations/tests/v2_link_wire.rs'},
  {package: 'fabric-sdk-items', target: 'links',
    source: 'rust/engine/control-plane/resources/items/tests/links.rs'},
];
const REGISTRATION_PROFILES = [
  {id: 'default', features: []},
  {id: 'headless', features: ['headless']},
  {id: 'v2-link-kql', features: ['v2-link-kql']},
  {id: 'v2-link-kql-refresh-headless',
    features: ['headless', 'runtime-v2', 'v2-link-kql', 'v2-refresh']},
];

function leafCommand(name) {
  const command = dataagent.leafCommand(name);
  if (name === KQL_TARGET) {
    command.args[command.args.indexOf('--features') + 1] = 'headless,v2-link-kql';
  }
  return command;
}

function mainCommand(module, features = ['headless', 'v2-link-kql']) {
  return {
    args: ['test', '--offline', '--locked', '-p', 'fabric-cli', '--no-default-features',
      '--features', features.join(','), '--bin', 'fab-cli-v2', `${module}::tests::`],
    required: update.registeredTests(`rust/clients/cli/app/src/${module}_tests.rs`)
      .map(name => `${module}::tests::${name}`),
    exhaustive: true,
  };
}

function contractCommand(target) {
  return {
    args: ['test', '--offline', '--locked', '-p', target.package, '--no-default-features',
      '--test', target.target],
    required: update.registeredTests(target.source),
    exhaustive: true,
  };
}

function registrationCommands() {
  return REGISTRATION_PROFILES.flatMap(({features}) => ['check', 'clippy'].map(kind => ({
    args: [kind, '--offline', '--locked', '-p', 'fabric-cli', '--all-targets', '--no-default-features',
      ...(features.length ? ['--features', features.join(',')] : []),
      ...(kind === 'clippy' ? ['--', '-D', 'warnings'] : [])],
  })));
}

const ENTRY = {
  policy: dataagent.KQL_POLICY,
  driver: 'rust/scripts/integration-kql-link-smoke.cjs',
  candidate: 'dev/marcozo/integrate-kql-shortcut-links-pr55-end-to-tomz4t',
  base: '61c8221914152e231e5e64f7bd38418f66264999',
  source: 'e1baaee16dc6eb4917c5310224c27fcd258564ef',
  qualification: 'candidate-only offline Windows KQL LINK and metadata-selected current-consumer proof',
  scope: 'explicit KQL LINK, shared LINK and current DataAgent/main consumer inputs, not the complete head diff',
  leaves: [...dataagentLink.ENTRY.leaves, KQL_TARGET],
  inputs: [...new Set([...dataagentLink.ENTRY.inputs,
    'rust/clients/cli/app/src/v2_link_dispatch.rs',
    'rust/clients/cli/app/src/v2_link_kql_host.rs',
    'rust/clients/cli/app/src/v2_link_kql_main.rs',
    'rust/clients/cli/app/src/v2_link_kql_main_tests.rs',
    'rust/clients/cli/app/src/v2_livy_list_main.rs',
    'rust/clients/cli/app/src/v2_refresh_main_tests.rs',
    `rust/clients/cli/app/tests/${KQL_TARGET}.rs`,
  ])],
  leafCommand,
  commands: [
    ...['v2_link_kql_main', 'v2_link_dataagent_main'].flatMap(module => [
      mainCommand(module),
      mainCommand(module, REGISTRATION_PROFILES.at(-1).features),
    ]),
    mainCommand('v2_refresh_main', REGISTRATION_PROFILES.at(-1).features),
    ...CONTRACT_TARGETS.map(contractCommand),
    ...registrationCommands(),
  ],
};

module.exports = {ENTRY, KQL_TARGET, CONTRACT_TARGETS, REGISTRATION_PROFILES,
  leafCommand, mainCommand, contractCommand, registrationCommands};
if (require.main === module) {
  try { assert.equal(process.argv.length, 3); dataagent.main(process.argv[2], ENTRY); }
  catch (error) { console.error(error.stack); process.exitCode = 1; }
}
