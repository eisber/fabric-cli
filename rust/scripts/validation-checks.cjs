// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const path = require('node:path');

const PARAMETER_SOURCE_COMMIT = '5ca096c00a9a878c646c59fe537b883eba951329';
const PYTHON_ENV = Object.freeze({PYTHONUTF8: '1', PYTHONDONTWRITEBYTECODE: '1'});
const DEFAULT_TRANSPORT_FORBIDDEN =
  /^(hyper|hyper-util|hyper-rustls|rustls|rustls-platform-verifier|reqwest|native-tls|openssl|fabric-identity|fabric-identity-windows|fabric-cli|fabric-deploy) v/m;
const OPTIN_TRANSPORT_FORBIDDEN =
  /^(reqwest|native-tls|openssl|fabric-identity|fabric-identity-windows|fabric-cli|fabric-deploy) v/m;

function requireThat(condition, message) {
  if (!condition) throw new Error(message);
}

function absolute(value, label) {
  requireThat(typeof value === 'string' && !/[\0\r\n]/.test(value) && path.isAbsolute(value),
    `${label} must be an absolute path`);
  return path.normalize(value);
}

function contained(root, value, label) {
  const resolved = absolute(value, label);
  const relative = path.relative(root, resolved);
  requireThat(relative && relative !== '..' && !relative.startsWith(`..${path.sep}`) &&
    !path.isAbsolute(relative), `${label} must be inside artifacts`);
  return resolved;
}

function context(options) {
  requireThat(options && typeof options === 'object', 'Check context is required');
  const root = absolute(options.root, 'root');
  const artifacts = absolute(options.artifacts, 'artifacts');
  const rust = path.join(root, 'rust');
  const pinnedOutput = contained(artifacts, options.pinnedOutput ??
    path.join(artifacts, 'pinned-python'), 'pinnedOutput');
  const pinnedRust = absolute(options.pinnedRust ??
    path.join(pinnedOutput, 'source', 'rust'), 'pinnedRust');
  requireThat(pinnedRust !== rust, 'Python references must not run in the candidate Rust tree');
  const parameterRoot = contained(artifacts, options.parameterRoot ??
    path.join(artifacts, 'fabric-cicd-parameter-source'), 'parameterRoot');
  const configuredTarget = options.targetDir ?? options.env?.CARGO_TARGET_DIR ??
    process.env.CARGO_TARGET_DIR;
  requireThat(configuredTarget === undefined || (typeof configuredTarget === 'string' &&
    configuredTarget.length > 0 && !/[\0\r\n]/.test(configuredTarget)),
  'targetDir must be a nonempty path');
  const targetDir = configuredTarget === undefined ? path.join(rust, 'target') :
    path.resolve(rust, configuredTarget);
  const cargoEnv = configuredTarget === undefined ? {} : {CARGO_TARGET_DIR: targetDir};
  const platform = options.platform ?? process.platform;
  const suffix = /^(win32|windows|windows-\d+)$/i.test(platform) ? '.exe' : '';
  const probes = path.join(targetDir, 'debug', 'examples');
  const file = (...parts) => path.join(...parts);
  const artifact = name => path.join(artifacts, name);
  const command = (program, args, extra = {}) => ({kind: 'command', program, args, cwd: rust, ...extra});
  const cargo = (...args) => command('cargo', args,
    Object.keys(cargoEnv).length ? {env: {...cargoEnv}} : {});
  const python = (args, frozen = true, extra = {}) => command(options.python ?? 'python',
    ['-B', ...args], {cwd: frozen ? pinnedRust : rust, env: {...PYTHON_ENV}, ...extra});
  const unit = (directory, pattern, frozen = true) => python([
    '-m', 'unittest', 'discover', '-s', file(...directory), '-p', pattern, '-v',
  ], frozen, {test: 'unittest'});
  const toolingUnit = (directory, pattern) => python([
    '-m', 'unittest', 'discover', '-s', file(rust, ...directory), '-p', pattern, '-v',
  ], false, {cwd: artifacts, test: 'unittest',
    env: {...PYTHON_ENV, TMPDIR: artifacts, TEMP: artifacts, TMP: artifacts}});
  const reference = (parts, ...args) => python([file(...parts), ...args]);
  const run = (pkg, example, quiet = false) => cargo('run', ...(quiet ? ['--quiet'] : []),
    '--offline', '--locked', '-p', pkg, '--example', example);
  const readParity = name => reference(['tests', 'compatibility', 'run_read_parity.py'],
    '--read-probe', file(probes, `read_probe${suffix}`),
    '--query-probe', file(probes, `projection_output${suffix}`),
    '--folder-probe', file(probes, `folder_read_probe${suffix}`),
    '--artifacts', artifact(name));
  return {root, rust, artifacts, pinnedOutput, pinnedRust, parameterRoot, probes, suffix,
    plan: options.plan, file, artifact, command, cargo, python, unit, toolingUnit,
    reference, run, readParity};
}

// Each entry owns the named legacy workflow step, not commands supplied by the
// impact graph. Empty factories are deliberate core-profile obligations.
const REGISTRY = Object.freeze({
  'selector-tests': {
    step: 'Affected-crate selection assertions and shared runner contracts',
    commands: c => [c.command('node', ['--test',
      c.file(c.rust, 'scripts', 'impact-plan.test.cjs'),
      c.file(c.rust, 'scripts', 'validate-impact.test.cjs'),
      c.file(c.rust, 'scripts', 'validation-checks.test.cjs'),
      c.file(c.rust, 'scripts', 'rust-fast-check.test.cjs'),
      c.file(c.rust, 'scripts', 'rust-coverage.test.cjs')], {
      cwd: c.artifacts,
      env: {IMPACT_TEST_ROOT: c.artifacts, TMPDIR: c.artifacts,
        TEMP: c.artifacts, TMP: c.artifacts},
    })],
  },
  'command-inventory': {
    step: 'Validate declared command acceptance targets against actual Cargo metadata',
    commands: c => {
      const metadata = c.artifact('command-inventory-metadata.json');
      return [
        {...c.cargo('metadata', '--offline', '--locked', '--no-deps', '--format-version', '1'),
          stdout: metadata},
        c.python([c.file('scripts', 'check_command_acceptance.py'),
          '--root', c.root, '--metadata', metadata], false),
      ];
    },
  },
  'oracle-tools': {
    step: 'Prepare isolated frozen Python source and candidate oracle overlays',
    commands: c => [
      ...['test_release_verify.py', 'test_native_acceptance*.py',
        'test_assemble_native_release.py', 'test_pinned_python_source.py',
        'test_check_command_acceptance.py', 'test_v2_upstream.py']
        .map(pattern => c.toolingUnit(['scripts'], pattern)),
      c.toolingUnit(['tests', 'acceptance', 'identity'], 'test_check_report.py'),
      c.python([c.file('scripts', 'pinned_python_source.py'), 'prepare',
        '--candidate-root', c.root, '--output', c.pinnedOutput], false, {
        kind: 'prepare-oracle', stdout: c.artifact('pinned-python-receipt.json'),
        pinnedOutput: c.pinnedOutput,
      }),
    ],
  },
  'batch-oracle': {
    step: 'Verify independent pinned batch observations',
    commands: c => [c.reference(['frontends', 'cli', 'app', 'tests', 'batch_reference.py'],
      '--check', c.file('frontends', 'cli', 'app', 'tests', 'batch_source.json'))],
  },
  transport: {
    step: 'Validate the explicitly selected native transport',
    // Hyper-rustls all-target check/clippy and selected tests belong to core.
    commands: c => [
      c.cargo('check', '--offline', '--locked', '-p', 'fabric-sdk-transport',
        '--no-default-features', '--lib'),
      c.cargo('run', '--offline', '--locked', '-p', 'fabric-sdk-transport',
        '--features', 'hyper-rustls', '--example', 'sdk_consumer'),
    ],
  },
  'transport-graphs': {
    step: 'Require isolated default and opt-in transport dependency graphs',
    commands: c => {
      const defaults = ['fabric-sdk', 'fabric-sdk-transport'].map(pkg => ({
        package: pkg, path: c.artifact(`${pkg}-default-transport-graph.txt`),
      }));
      const optin = c.artifact('fabric-sdk-transport-optin-graph.txt');
      return [
        ...defaults.map(graph => ({...c.cargo('tree', '--offline', '--locked',
          '-p', graph.package, '--no-default-features', '--edges', 'normal',
          '--prefix', 'none', '--format', '{p}'), stdout: graph.path})),
        {...c.cargo('tree', '--offline', '--locked', '-p', 'fabric-sdk-transport',
          '--features', 'hyper-rustls', '--edges', 'normal', '--prefix', 'none',
          '--format', '{p}'), stdout: optin},
        c.command('node', [], {kind: 'transport-graphs', graphs: {defaults, optin}}),
      ];
    },
  },
  'read-parity': {
    step: 'Compare executable read and query behavior against pinned Python',
    commands: c => [
      c.cargo('build', '--offline', '--locked', '-p', 'fabric-cli', '-p',
        'fabric-cli-query', '--examples'),
      c.unit(['tests', 'compatibility'], 'test_run_read_parity.py'),
      c.unit(['tests', 'compatibility', 'commands'], 'test_inventory.py'),
      c.readParity('read-parity'),
    ],
  },
  'json-parity': {
    step: 'Require shared core and CLI JSON byte compatibility',
    commands: c => {
      const observations = [
        ['fabric-core-json', 'core_definition_json', 'core-definition-json.json', 'definition_reference.py'],
        ['fabric-cli-output', 'definition_json', 'cli-definition-json.json', 'definition_reference.py'],
        ['fabric-cli-output', 'compatibility_probe', 'output-json.json', 'reference.py'],
        ['fabric-core-json', 'unicode_json', 'unicode-json.json', 'unicode_reference.py'],
        ['fabric-core-json', 'native_json', 'native-json.json', 'native_reference.py'],
        ['fabric-cli-output', 'text_probe', 'core-text-observations.json', 'text_reference.py'],
      ];
      return [
        ...observations.map(([pkg, example, output]) => ({
          ...c.run(pkg, example, true), stdout: c.artifact(output),
        })),
        ...observations.map(([, example, output, script]) => c.reference(
          ['tests', 'compatibility', 'output', script],
          ...(example === 'native_json' ? ['--encoding-only'] : []),
          '--observed', c.artifact(output))),
        ...['test_reference.py', 'test_unicode_reference.py', 'test_native_reference.py']
          .map(pattern => c.unit(['tests', 'compatibility', 'output'], pattern)),
      ];
    },
  },
  broker: {
    step: 'Check optional broker and safe-code consumer without authenticating',
    // Core: fabric-cli and fabric-identity with fabric-cli/windows-broker;
    // fabric-identity/windows-broker all-target compile, clippy and tests.
    commands: c => [
      c.cargo('run', '--offline', '--locked', '-p', 'fabric-identity',
        '--example', 'safe_consumer', '--no-default-features'),
      c.cargo('run', '--offline', '--locked', '-p', 'fabric-identity',
        '--example', 'safe_consumer', '--features', 'windows-broker'),
    ],
  },
  repl: {
    step: 'Check optional removable terminal host without terminal or identity access',
    // Core: fabric-cli + fabric-cli-repl, feature fabric-cli/repl.
    commands: c => [c.unit(['frontends', 'cli', 'shell', 'tests'],
      'test_source_oracle.py', false)],
  },
  interactive: {
    step: 'Check explicit device-code credentials and native host without live authentication',
    // Core: fabric-cli/interactive and fabric-identity/interactive, including
    // provider tests and feature-forwarding CLI consumers in the selected profile.
    commands: () => [],
  },
  headless: {
    step: 'Check explicit headless credentials and native host without live authentication',
    // Core: fabric-identity/headless all-target tests; fabric-cli/headless
    // with fab-rs, headless_graph, headless_configuration, cli_contract, argv_contract.
    // Provider packages compile in the profile; their tests follow changed
    // provider components, not unrelated CLI-only profile/package overlap.
    commands: () => [],
  },
  certificate: {
    step: 'Check certificate credentials without live authentication',
    // Core: fabric-identity/certificate and fabric-cli/certificate, including
    // the safe unit and native-provider contracts. No standalone legacy command.
    commands: () => [],
  },
  updater: {
    step: 'Check signed update engine and optional standalone host offline',
    // Core: fabric-cli-update/fixture-harness all targets, CLI self-update;
    // update_commands, cli_contract, argv_contract, workspace_contract, lib, fab-rs.
    // Core must set FAB_UPDATE_TEST_ROOT to the owned artifact root.
    commands: () => [],
  },
  combined: {
    step: 'Check combined native, headless, terminal and update composition offline',
    // Core: CLI native-read,headless,repl,self-update together, plus identity;
    // fab-rs, update_commands, headless_graph, headless_configuration,
    // cli_contract, argv_contract, workspace_contract; FAB_UPDATE_TEST_ROOT set.
    commands: () => [],
  },
  'jobs-oracle': {
    step: 'Require pinned Python job contracts and actual offline job consumer',
    commands: c => [
      ...['reference', 'branches_reference', 'operation_output_reference',
        'skill_headers_reference', 'command_source', 'poll_source',
        'numeric_source', 'endpoint_reference'].map(name =>
        c.reference(['tests', 'compatibility', 'jobs', `${name}.py`])),
      c.run('fabric-cli-jobs', 'offline_job'),
    ],
  },
  'definitions-oracle': {
    step: 'Require released definition source receipts and native transfer consumers',
    commands: c => {
      // Check-only source changes can select this oracle without selecting its
      // native consumer. Deduplicate only a complete default-mode core test.
      const covered = (c.plan?.tests ?? []).some(test =>
        test.package === 'fabric-deploy' && ['all', 'test:definitions'].includes(test.target) &&
        test.filter === undefined && test.default_available !== false &&
        !test.default_skipped_targets?.includes('test:definitions'));
      return [
        c.unit(['tests', 'compatibility'], 'test_run_definition_parity.py'),
        c.reference(['tests', 'compatibility', 'run_definition_parity.py'],
          '--artifacts', c.artifact('definition-parity')),
        ...(covered ? [] : [c.cargo('test', '--offline', '--locked', '-p',
          'fabric-deploy', '--test', 'definitions')]),
      ];
    },
  },
  'import-oracle': {
    step: 'Require immutable import input and Environment lifecycle source receipts',
    commands: c => [c.reference(['sdk', 'deployment',
      'tests', 'definition_import_reference_checks.py'])],
  },
  'parameters-oracle': {
    step: 'Require immutable configuration and scoped parameter source receipts',
    commands: c => [
      c.command('git', ['init', c.parameterRoot], {env: {GIT_TERMINAL_PROMPT: '0'}}),
      c.command('git', ['-C', c.parameterRoot, 'config', '--local', 'core.autocrlf', 'true']),
      c.command('git', ['-C', c.parameterRoot, 'fetch', '--depth=1',
        'https://github.com/microsoft/fabric-cicd.git', PARAMETER_SOURCE_COMMIT],
      {env: {GIT_TERMINAL_PROMPT: '0'}}),
      c.command('git', ['-C', c.parameterRoot, 'checkout', '--detach', PARAMETER_SOURCE_COMMIT]),
      ...['configured_flags', 'flag_protocol', 'config_value_edges', 'parameter_context',
        'parameter_pipeline', 'path_filter', 'selected_constants', 'parameter_resolution',
        'parameter_schema_edges', 'config_extensions_execution'].map(name =>
        c.reference(['sdk', 'deployment', 'tests',
          `${name}_reference.py`], '--reference-root', c.parameterRoot)),
    ],
  },
  'export-oracle': {
    step: 'Require immutable export storage and metadata source receipts',
    commands: c => [c.reference(['tests', 'compatibility', 'run_export_source_parity.py'],
      '--artifacts', c.file(c.pinnedRust, '..', '.oracle-artifacts', 'export-source-receipts'))],
  },
  'native-read': {
    step: 'Require opt-in native read host without authentication',
    // Core must retain BOTH native-read and native-read,repl feature profiles.
    // The latter tests fab-rs, cli_contract and argv_contract independently of
    // the four-feature combined profile; it is not equivalent to that profile.
    commands: c => [
      c.cargo('build', '--offline', '--locked', '-p', 'fabric-cli', '--examples',
        '--features', 'native-read'),
      c.reference(['tests', 'compatibility', 'read_session', 'consumer.py'],
        '--rust-probe', c.file(c.probes, `read_session_probe${c.suffix}`)),
      c.reference(['tests', 'compatibility', 'job_reads', 'compare.py'],
        '--probe', c.file(c.probes, `job_read_session_probe${c.suffix}`)),
    ],
  },
  'context-output': {
    step: 'Require extended context and actual text output observations',
    commands: c => [
      c.run('fabric-cli-context', 'extended_resolve', true),
      c.reference(['tests', 'compatibility', 'context', 'extended_reference.py']),
      {...c.run('fabric-cli-output', 'text_probe', true),
        stdout: c.artifact('text-observations.json')},
      c.reference(['tests', 'compatibility', 'output', 'text_reference.py'],
        '--observed', c.artifact('text-observations.json')),
    ],
  },
  'broker-read-parity': {
    step: 'Preserve read and query source comparisons with broker feature enabled',
    commands: c => [
      c.cargo('build', '--offline', '--locked', '-p', 'fabric-cli', '-p',
        'fabric-cli-query', '--examples', '--features', 'fabric-cli/windows-broker'),
      c.readParity('read-parity-windows-broker'),
    ],
  },
  'sdk-example': {
    step: 'Run independent offline SDK example',
    commands: c => [c.run('fabric-sdk', 'injected_credentials')],
  },
  'resource-oracle': {
    step: 'Compare resource operations and errors with pinned Python consumers',
    commands: c => [
      c.cargo('build', '--offline', '--locked', '-p', 'fabric-cli-mutations',
        '-p', 'fabric-sdk-operations', '--examples'),
      c.unit(['tests', 'compatibility'], 'test_run_resource_parity.py'),
      c.reference(['tests', 'compatibility', 'run_resource_parity.py'],
        '--probes', c.probes, '--artifacts', c.artifact('resource-parity')),
    ],
  },
  'cli-smoke': {
    step: 'Check CLI help; Check CLI version',
    commands: c => ['--help', '--version'].map(flag =>
      c.cargo('run', '--offline', '--locked', '-p', 'fabric-cli', '--', flag)),
  },
  'sdk-graph': {
    step: 'Inspect SDK normal dependency closure',
    commands: c => [c.cargo('tree', '--locked', '-p', 'fabric-sdk', '--edges', 'normal')],
  },
  'dependency-graph': {
    step: 'Inspect dependency features and duplicates',
    commands: c => {
      const packages = c.plan?.packages;
      requireThat(Array.isArray(packages) && packages.length > 0 &&
        packages.every(name => typeof name === 'string' && /^[a-z0-9][a-z0-9_-]*$/i.test(name)) &&
        new Set(packages).size === packages.length, 'Dependency graph requires selected package names');
      const selected = packages.flatMap(name => ['--package', name]);
      return [
        c.cargo('tree', '--locked', ...selected, '--edges', 'features'),
        c.cargo('tree', '--locked', ...selected, '--duplicates'),
      ];
    },
  },
  'frozen-verify': {
    step: 'Verify frozen source and candidate oracle overlays remain unchanged',
    // The executor schedules this in finally after successful prepare, including
    // when an intervening contract fails. It must not be a normal skipped tail.
    commands: c => [c.python([c.file('scripts', 'pinned_python_source.py'),
      'verify', '--output', c.pinnedOutput], false, {kind: 'verify-oracle'})],
  },
});

const CHECK_IDS = Object.freeze(Object.keys(REGISTRY));
const CHECK_BUDGETS = Object.freeze({
  'parameters-oracle': 300000,
  'export-oracle': 300000,
  'resource-oracle': 300000,
});

/** Return trusted argv descriptors without touching files or invoking commands. */
function commandsFor(id, options) {
  requireThat(Object.hasOwn(REGISTRY, id), `Unknown validation check: ${id}`);
  return REGISTRY[id].commands(context(options));
}

/** Reject missing, duplicate and unknown graph checks, including profile markers. */
function validateRegistry(graph) {
  requireThat(graph && Array.isArray(graph.checks), 'Impact graph checks must be an array');
  const ids = graph.checks.map(check => check?.id);
  requireThat(ids.every(id => typeof id === 'string'), 'Impact graph checks require IDs');
  requireThat(new Set(ids).size === ids.length, 'Impact graph has duplicate check IDs');
  const unknown = ids.filter(id => !Object.hasOwn(REGISTRY, id));
  const missing = CHECK_IDS.filter(id => !ids.includes(id));
  requireThat(!unknown.length && !missing.length,
    `Validation registry mismatch; unknown: ${unknown.join(', ') || 'none'}; missing: ${missing.join(', ') || 'none'}`);
  return true;
}

/** Preserve the legacy grep -Eq line predicates on captured Cargo tree output. */
function assertTransportGraphs(defaults, optin) {
  requireThat(defaults && typeof defaults === 'object' && !Array.isArray(defaults),
    'Default transport graphs must be keyed by package name');
  for (const pkg of ['fabric-sdk', 'fabric-sdk-transport']) {
    requireThat(typeof defaults[pkg] === 'string', `Missing default ${pkg} graph`);
    requireThat(new RegExp(`^${pkg} v`, 'm').test(defaults[pkg]),
      `Missing default ${pkg} package in captured graph`);
    requireThat(!DEFAULT_TRANSPORT_FORBIDDEN.test(defaults[pkg]),
      `Default ${pkg} graph selected a backend or higher layer`);
  }
  requireThat(typeof optin === 'string', 'Missing opt-in transport graph');
  for (const pkg of ['hyper', 'hyper-util', 'hyper-rustls', 'rustls']) {
    requireThat(new RegExp(`^${pkg} v`, 'm').test(optin),
      `Opt-in transport graph is missing ${pkg}`);
  }
  requireThat(!OPTIN_TRANSPORT_FORBIDDEN.test(optin),
    'Opt-in transport selected an unrelated backend or higher layer');
  return true;
}

module.exports = {CHECK_IDS, CHECK_BUDGETS, commandsFor, validateRegistry, assertTransportGraphs};
