// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {spawnSync} = require('node:child_process');
const {test} = require('node:test');
const {plan} = require('./impact-plan.cjs');
const dataagent = require('./integration-dataagent-smoke.cjs');
const dataagentLink = require('./integration-dataagent-link-smoke.cjs');
const notebook = require('./integration-notebook-smoke.cjs');
const ocv = require('./integration-ocv-smoke.cjs');
const update = require('./integration-update-smoke.cjs');
const kql = require('./integration-kql-link-smoke.cjs');

const ROOT = path.resolve(__dirname, '../..');
const read = file => fs.readFileSync(path.join(ROOT, file), 'utf8');
const graph = () => JSON.parse(read('rust/validation/impact-graph.json'));
const features = ({args}) => args.includes('--features') ? args[args.indexOf('--features') + 1].split(',') : [];
const output = (names, filtered = 0) => names.map(name => `test ${name} ... ok\n`).join('') +
  `test result: ok. ${names.length} passed; 0 failed; 0 ignored; 0 measured; ${filtered} filtered out\n`;

// Lexical scope check only; actual Rust compilation/registration remains required.
function rustCode(source) {
  const code = source.split('');
  const hide = (start, end) => {
    for (let index = start; index < end; index++) {
      if (!/[\r\n]/.test(code[index])) code[index] = ' ';
    }
  };
  for (let index = 0; index < source.length;) {
    const start = index;
    if (source.startsWith('//', index)) {
      const end = source.indexOf('\n', index + 2);
      index = end < 0 ? source.length : end;
    } else if (source.startsWith('/*', index)) {
      let depth = 1;
      index += 2;
      while (depth && index < source.length) {
        if (source.startsWith('/*', index)) { depth++; index += 2; }
        else if (source.startsWith('*/', index)) { depth--; index += 2; }
        else index++;
      }
      assert.equal(depth, 0, 'Unterminated Rust block comment');
    } else {
      const raw = source.slice(index).match(/^(?:b|c)?r(#{0,255})"/);
      const quoted = source.slice(index).match(/^(?:b|c)?"/);
      const character = source.slice(index).match(
        /^b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\r\n])|[^'\\\r\n])'/u,
      );
      if (raw) {
        const end = source.indexOf(`"${raw[1]}`, index + raw[0].length);
        assert.ok(end >= 0, 'Unterminated Rust raw string');
        index = end + raw[1].length + 1;
      } else if (quoted) {
        index += quoted[0].length;
        while (index < source.length && source[index] !== '"') {
          index += source[index] === '\\' ? 2 : 1;
        }
        assert.ok(index < source.length, 'Unterminated Rust string');
        index++;
      } else if (character) index += character[0].length;
      else { index++; continue; }
    }
    hide(start, index);
  }
  return code.join('');
}

function testDeclarationScopes(source) {
  const code = rustCode(source), depths = [];
  let depth = 0;
  for (let index = 0; index < code.length; index++) {
    depths[index] = depth;
    if (code[index] === '{') depth++;
    if (code[index] === '}') depth--;
    assert.ok(depth >= 0, 'Unbalanced Rust braces');
  }
  assert.equal(depth, 0, 'Unbalanced Rust braces');
  const tests = [...code.matchAll(
    /#\[(?:tokio::)?test(?:\([^\]]*\))?\]\s*(?:async\s+)?fn\s+(\w+)/g,
  )].map(match => ({name: match[1], depth: depths[match.index]}));
  assert.ok(tests.length > 0, 'No lexical test declarations');
  return tests;
}

function moduleTestDeclarations(source) {
  const tests = testDeclarationScopes(source);
  for (const test of tests) assert.equal(test.depth, 0, `Test is not at module scope: ${test.name}`);
  return tests.map(test => test.name);
}

function rustTestItem(source, name) {
  const text = source.replace(/\r\n/g, '\n'), code = rustCode(text);
  const declaration = [...code.matchAll(
    /#\[(?:tokio::)?test(?:\([^\]]*\))?\]\s*(?:async\s+)?fn\s+(\w+)/g,
  )].find(match => match[1] === name);
  assert.ok(declaration, `Missing test declaration: ${name}`);
  const start = text.lastIndexOf('\n', declaration.index) + 1;
  const opening = code.indexOf('{', declaration.index);
  assert.ok(opening >= 0);
  let end = opening + 1, depth = 1;
  for (; depth && end < code.length; end++) {
    if (code[end] === '{') depth++;
    if (code[end] === '}') depth--;
  }
  assert.equal(depth, 0);
  return {text: text.slice(start, end), remaining: text.slice(0, start) + text.slice(end)};
}

test('KQL host regressions are module-scoped siblings, not nested declarations counted as registrations', () => {
  const file = 'rust/clients/cli/app/tests/v2_link_kql_host.rs';
  const historical = spawnSync('git', ['show', `ac93cd06f554c3b1a609c79ed0d55260be32ea64:${file}`], {
    cwd: ROOT, encoding: 'utf8', windowsHide: true, timeout: 30000,
  });
  assert.equal(historical.error, undefined);
  assert.equal(historical.status, 0);
  assert.throws(() => moduleTestDeclarations(historical.stdout),
    /Test is not at module scope: native_invalid_shortcut_names_reject_before_open_stdin_or_authentication/);
  assert.deepEqual(moduleTestDeclarations(read(file)), update.registeredTests(file));
  assert.match(read(file), /#!\[deny\(unnameable_test_items\)\]/);
  const regression = source => {
    const text = source.replace(/\r\n/g, '\n'), code = rustCode(text);
    const declaration = code.indexOf(
      'fn native_invalid_shortcut_names_reject_before_open_stdin_or_authentication(');
    assert.ok(declaration >= 0);
    const attribute = text.lastIndexOf('#[cfg(feature = "headless")]', declaration);
    assert.match(text.slice(attribute, declaration),
      /^#\[cfg\(feature = "headless"\)\]\s*#\[test\]\s*$/);
    const start = text.lastIndexOf('\n', attribute) + 1;
    let end = code.indexOf('{', declaration), depth = 1;
    assert.ok(end >= 0);
    for (end++; depth && end < code.length; end++) {
      if (code[end] === '{') depth++;
      if (code[end] === '}') depth--;
    }
    assert.equal(depth, 0);
    return {text: text.slice(start, end), remaining: text.slice(0, start) + text.slice(end)};
  };
  const before = regression(historical.stdout), after = regression(read(file));
  assert.equal(after.text, before.text.replace(/^ {8}/gm, ''),
    'Move only indentation and lexical placement; keep the entire cfg, body and assertions');
  const nonblank = text => text.split('\n').filter(line => line.trim()).join('\n');
  assert.equal(nonblank(after.remaining), nonblank(before.remaining),
    'Every other nonblank source line, including outer test semantics and lint, is unchanged');
});

test('both KQL host and main test files keep every declaration at module scope after the 2f5c main failure', () => {
  const main = 'rust/clients/cli/app/src/v2_link_kql_main_tests.rs';
  const host = 'rust/clients/cli/app/tests/v2_link_kql_host.rs';
  const historical = spawnSync('git', ['show', `2f5c25df1e8e7ce0a4b17e098550229bb24490e6:${main}`], {
    cwd: ROOT, encoding: 'utf8', windowsHide: true, timeout: 30000,
  });
  assert.equal(historical.error, undefined);
  assert.equal(historical.status, 0);
  const stalled = 'stalled_stderr_delivery_retains_accepted_handle_with_bounded_return';
  const owned = 'owned_stderr_worker_cannot_prevent_supervised_process_exit_after_deadline';
  const previous = testDeclarationScopes(historical.stdout);
  assert.equal(previous.find(test => test.name === stalled).depth, 1, 'Old async regression is in a function');
  assert.equal(previous.find(test => test.name === owned).depth, 3, 'Old sync regression is in the nested Write impl');
  assert.throws(() => moduleTestDeclarations(historical.stdout), /Test is not at module scope/);
  for (const file of [host, main]) {
    assert.deepEqual(moduleTestDeclarations(read(file)), update.registeredTests(file));
    assert.match(read(file), /#!\[deny\(unnameable_test_items\)\]/);
  }
  assert.deepEqual(moduleTestDeclarations(read(main)), previous.map(test => test.name),
    'No previous test declaration is lost, renamed or reordered');
  const beforeOwned = rustTestItem(historical.stdout, owned);
  const afterOwned = rustTestItem(read(main), owned);
  assert.equal(afterOwned.text, beforeOwned.text.replace(/^ {12}/gm, ''),
    'The entire process-test attribute/body/assertions move by exact deindent only');
  const beforeStalled = rustTestItem(beforeOwned.remaining, stalled);
  const afterStalled = rustTestItem(afterOwned.remaining, stalled);
  const nonblank = text => text.replace(/\r\n/g, '\n').split('\n').filter(line => line.trim()).join('\n');
  assert.equal(nonblank(afterStalled.text), nonblank(beforeStalled.text.replace(/^ {4}/gm, '')),
    'Async attribute/body and both Write fixtures remain intact after extracting the inner test');
  assert.equal(nonblank(afterStalled.remaining), nonblank(beforeStalled.remaining),
    'Every other nonblank main-test line, including outer classification loop and deny lint, is unchanged');
});

test('lexical test placement ignores Rust comments and literals but rejects nested blocks regardless of indentation', () => {
  const siblings = [
    '// } #[test] fn fake_line() {}',
    '/* { /* } */ #[test] fn fake_block() {} */',
    'const RAW: &str = br##"} #[test] fn fake_raw() { //"##;',
    'const TEXT: &str = "} \\" #[test] fn fake_string() {}";',
    "const CHARACTER: char = '}';",
    "const BYTE: u8 = b'{';",
    "fn borrowed<'a>(value: &'a str) -> &'a str { value }",
    '#[test] fn first() { let _ = format!("{value}"); }',
    '#[tokio::test] async fn second() {}',
  ].join('\n');
  assert.deepEqual(moduleTestDeclarations(siblings), ['first', 'second']);
  for (const opening of ['fn outer() {', 'fn outer() { for _ in 0..1 {']) {
    const closing = opening.includes('for ') ? '}}' : '}';
    assert.throws(() => moduleTestDeclarations(`${opening}\n${siblings}\n${closing}`),
      /Test is not at module scope: first/);
  }
  for (const opening of ['impl Write for Stalled {', 'fn outer() { impl Write for Stalled {']) {
    const closing = opening.startsWith('fn') ? '}}' : '}';
    assert.throws(() => moduleTestDeclarations(`${opening}\n${siblings}\n${closing}`),
      /Test is not at module scope: first/);
  }
  for (const malformed of ['/*', 'const A: &str = r#"open;', 'const A: &str = "open;',
    '#[test] fn test() {', '} #[test] fn test() {}']) {
    assert.throws(() => moduleTestDeclarations(malformed), /Unterminated|Unbalanced/);
  }
});

test('PR55 proof is candidate-only, immutable-pinned, and retains the existing finite hosted runner', () => {
  const workflow = read('.github/workflows/integration-smoke.yml').replace(/\r\n/g, '\n');
  const block = workflow.split("      - name: Check the PR55 candidate's ")[1].split('\n      - name:')[0];
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  for (const [event, ref, expected] of [
    ['pull_request', kql.ENTRY.candidate, true], ['push', kql.ENTRY.candidate, false],
    ['pull_request', `${kql.ENTRY.candidate}-other`, false], ['schedule', kql.ENTRY.candidate, false],
  ]) {
    assert.equal(vm.runInNewContext(expression, {
      github: {event_name: event, event: {pull_request: {head: {ref}}}},
    }, {timeout: 1000}), expected);
  }
  assert.equal(kql.ENTRY.base, '61c8221914152e231e5e64f7bd38418f66264999');
  assert.equal(kql.ENTRY.source, 'e1baaee16dc6eb4917c5310224c27fcd258564ef');
  assert.match(block, /timeout-minutes: 20/);
  assert.match(workflow, /timeout-minutes: 45/);
  assert.ok(workflow.indexOf("Record the KQL candidate's first workflow-step time") <
    workflow.indexOf('uses: actions/checkout@'));
  assert.match(workflow, /kind: 'estimated-first-workflow-step'/);
  assert.match(workflow, /\{flag: 'wx'\}/);
  const otherCandidates = workflow.split('\n      - name: Check the ').slice(1)
    .filter(candidate => !candidate.startsWith("PR55 candidate's "));
  for (const other of otherCandidates) {
    assert.match(other.split('\n      - name:')[0], /timeout-minutes: 10/);
  }
  assert.match(block, /integration-kql-link-smoke\.cjs/);
  assert.ok(!block.includes('continue-on-error'));
  for (const binding of ['FAB_CLI_V2_TEST_ROOT', 'FAB_IDENTITY_TEST_ROOT', 'FAB_UPDATE_TEST_ROOT']) {
    assert.ok(block.includes(`${binding}:`));
  }
  assert.ok(workflow.includes('rust/scripts/integration-kql-link-smoke.test.cjs'));
  const runner = read('rust/scripts/integration-dataagent-smoke.cjs');
  assert.match(runner, /entry\.leaves\.map\(entry\.leafCommand \|\| leafCommand\)/);
  assert.equal((runner.match(/phase = phaseBudget\(/g) || []).length, 1);
  assert.match(runner, /event\.number, 106/);
  for (const guard of ['570000', 'github-hosted', 'metadata', '--offline', '--locked',
    'selection.json', 'selected.deferred', 'source-before.json', 'source-after.json', 'requireUnchanged']) {
    assert.ok(runner.includes(guard));
  }
  assert.match(read(kql.ENTRY.driver), /dataagent\.main\(process\.argv\[2\], ENTRY\)/);
});

test('only the exact KQL entry can select the admitted phase and immutable limits', () => {
  for (const entry of [{}, dataagentLink.ENTRY, {candidate: dataagent.CANDIDATE,
    base: dataagent.BASE, source: dataagent.SOURCE}]) {
    const policy = dataagent.qualificationPolicy(entry);
    assert.equal(policy, dataagent.DEFAULT_POLICY);
    assert.equal(policy.executorMilliseconds, 570000);
    assert.equal(policy.workflowStepMinutes, 10);
    assert.equal(policy.commandMaximumMilliseconds, 480000);
  }
  assert.equal(dataagent.qualificationPolicy(kql.ENTRY), dataagent.KQL_POLICY);
  assert.deepEqual(kql.ENTRY.policy, {
    id: 'kql-link-full-qualification-v2', phase: 'candidate-qualification',
    executorMilliseconds: 1140000, commandMaximumMilliseconds: 480000,
    workflowStepMinutes: 20, enclosingJobMinutes: 45, cargoWorkInventoryMaximum: 64,
    maximumAnchorAgeMilliseconds: 1200000, estimatedCleanupMarginMilliseconds: 180000,
  });
  assert.ok(Object.isFrozen(dataagent.KQL_POLICY) && Object.isFrozen(dataagent.DEFAULT_POLICY));
  for (const policy of [undefined, null, 'kql-link-full-qualification-v2',
    {...kql.ENTRY.policy, id: 'unknown'},
    {...kql.ENTRY.policy, phase: 'second-attempt'},
    {...kql.ENTRY.policy, executorMilliseconds: 1200000},
    {...kql.ENTRY.policy, commandMaximumMilliseconds: 480001},
    {...kql.ENTRY.policy, cargoWorkInventoryMaximum: 65},
    {...kql.ENTRY.policy, enclosingJobMinutes: 46},
    {...kql.ENTRY.policy, maximumAnchorAgeMilliseconds: 1200001},
    {...kql.ENTRY.policy, estimatedCleanupMarginMilliseconds: 179999},
    {...kql.ENTRY.policy, budgetFromEnvironment: true}]) {
    assert.throws(() => dataagent.qualificationPolicy({...kql.ENTRY, policy}), /Unrecognized/);
  }
  for (const key of ['candidate', 'base', 'source', 'driver']) {
    assert.throws(() => dataagent.qualificationPolicy({...kql.ENTRY, [key]: 'other'}), /not admitted/);
  }
});

test('one absolute phase deadline covers commands, failed work, overhead and finalization without renewal', () => {
  let now = 100000;
  for (const [entry, duration] of [[{}, 570000], [kql.ENTRY, 1140000]]) {
    const start = now;
    const phase = dataagent.phaseBudget(entry, () => now, start);
    assert.equal(phase.started, start);
    assert.equal(phase.deadline, start + duration);
    now += 1000;
    assert.equal(phase.commandTimeout(), 30000);
    assert.equal(phase.commandTimeout(480000), 480000);
    for (const invalid of [0, -1, 480001, NaN, Infinity, '480000']) {
      assert.throws(() => phase.commandTimeout(invalid), /Invalid command timeout/);
    }
    // A previous command failure grants no new clock or retry budget.
    now = phase.deadline - 7000;
    assert.equal(phase.commandTimeout(480000), 7000);
    assert.equal(phase.remaining(), 7000);
    assert.equal(phase.deadline, start + duration);
    now += 7000;
    for (let attempt = 0; attempt < 2; attempt++) {
      assert.throws(() => phase.commandTimeout(), /budget expired/);
      assert.throws(() => phase.remaining(), /budget expired/);
      assert.equal(phase.deadline, start + duration);
    }
  }
});

function anchorFixture(recorded = 1700000) {
  const context = {
    repository: 'eisber/fabric-cli', run_id: '44043745329', run_attempt: '1',
    source: 'a'.repeat(40), base: kql.ENTRY.base, candidate: kql.ENTRY.candidate,
    checkout: 'b'.repeat(40), original_source: kql.ENTRY.source,
  };
  return {context, anchor: {schema: 1, kind: 'estimated-first-workflow-step',
    recorded_unix_ms: recorded, ...context}};
}

test('estimated anchor admission retains unknown provider headroom and exact 20-minute/3-minute boundaries', () => {
  let now = 2000000;
  const phase = dataagent.phaseBudget(kql.ENTRY, () => now);
  const {anchor, context} = anchorFixture();
  const admission = dataagent.estimatedAnchorAdmission(phase, anchor, context, now);
  assert.equal(admission.estimated_admission, true);
  assert.equal(admission.estimated_remaining_milliseconds, 2400000);
  assert.equal(admission.age_at_phase_entry_milliseconds, 300000);
  assert.equal(admission.estimated_cleanup_margin_milliseconds, 180000);
  for (const key of ['provider_job_start_unix_ms', 'pre_anchor_elapsed_milliseconds',
    'true_provider_headroom_milliseconds']) assert.equal(admission[key], null);
  assert.equal(admission.provider_job_deadline_verified, false);
  assert.match(admission.censor_risk, /Unknown pre-anchor overhead/);
  for (const age of [0, 1200000]) {
    const fixture = anchorFixture(now - age);
    assert.equal(dataagent.estimatedAnchorAdmission(phase, fixture.anchor, fixture.context, now)
      .age_at_phase_entry_milliseconds, age);
  }
  assert.throws(() => dataagent.estimatedAnchorAdmission(phase,
    {...anchor, recorded_unix_ms: now - 1200001}, context, now), /stale/);
  assert.throws(() => dataagent.estimatedAnchorAdmission(phase, anchor, context, now - 1), /backwards/);
  assert.throws(() => dataagent.estimatedAnchorAdmission(phase, anchor, context,
    anchor.recorded_unix_ms + 1380001), /Insufficient estimated time/);
  now += 1139000;
  assert.equal(phase.remaining(), 1000);
  assert.equal(dataagent.estimatedAnchorAdmission(phase, anchor, context, now)
    .age_at_phase_entry_milliseconds, 300000, 'Later work does not renew or re-age the entry');
  now += 1000;
  assert.throws(() => dataagent.estimatedAnchorAdmission(phase, anchor, context, now), /budget expired/);
});

test('missing, malformed, future and mismatched immutable source/run/attempt anchors fail closed', () => {
  const now = 2000000;
  const phase = dataagent.phaseBudget(kql.ENTRY, () => now);
  const {anchor, context} = anchorFixture();
  for (const invalid of [undefined, null, '', [], {...anchor, schema: 2}, {...anchor, kind: 'provider-start'},
    {...anchor, extra: true}, Object.fromEntries(Object.entries(anchor).filter(([key]) => key !== 'source'))]) {
    assert.throws(() => dataagent.estimatedAnchorAdmission(phase, invalid, context, now));
  }
  for (const recorded of [undefined, null, '1700000', NaN, Infinity, -1, 0, 1.5, now + 1]) {
    assert.throws(() => dataagent.estimatedAnchorAdmission(phase,
      {...anchor, recorded_unix_ms: recorded}, context, now), /timestamp/);
  }
  for (const key of Object.keys(context)) {
    assert.throws(() => dataagent.estimatedAnchorAdmission(phase,
      {...anchor, [key]: 'other'}, context, now), new RegExp(`binding mismatch: ${key}`));
  }
  for (const key of ['run_id', 'run_attempt', 'source', 'base', 'checkout', 'original_source']) {
    assert.throws(() => dataagent.estimatedAnchorAdmission(phase,
      {...anchor, [key]: ''}, {...context, [key]: ''}, now));
  }
});

test('workflow records its bound estimate exclusively before checkout and changed bytes never reset a phase', () => {
  const workflow = read('.github/workflows/integration-smoke.yml').replace(/\r\n/g, '\n');
  const script = workflow.match(/run: \|\n          @'\n([\s\S]*?)\n          '@ \| node/)[1]
    .replace(/^          /gm, '');
  const {context} = anchorFixture();
  let contents;
  const env = {
    RUNNER_TEMP: '/hosted-temp', GITHUB_EVENT_PATH: '/event.json',
    GITHUB_REPOSITORY: context.repository, GITHUB_RUN_ID: context.run_id,
    GITHUB_RUN_ATTEMPT: context.run_attempt, GITHUB_SHA: context.checkout,
  };
  const sandbox = {Date: {now: () => 1700000}, process: {env},
    require(name) {
      if (name === 'node:path') return path.posix;
      assert.equal(name, 'node:fs');
      return {readFileSync(file) {
        assert.equal(file, env.GITHUB_EVENT_PATH);
        return JSON.stringify({pull_request: {head: {sha: context.source, ref: context.candidate},
          base: {sha: context.base}}});
      }, writeFileSync(file, bytes, options) {
        assert.equal(file, '/hosted-temp/kql-link-first-step-anchor.json');
        assert.equal(options.flag, 'wx');
        assert.equal(contents, undefined, 'Exclusive creation prevents a changed/reset anchor');
        contents = bytes;
      }};
    }};
  vm.runInNewContext(script, sandbox, {timeout: 1000});
  const phase = dataagent.phaseBudget(kql.ENTRY, () => 2000000);
  assert.equal(dataagent.estimatedAnchorAdmission(phase, JSON.parse(contents), context, 2000000)
    .estimated_admission, true);
  assert.throws(() => vm.runInNewContext(script, {...sandbox}, {timeout: 1000}), /Exclusive creation/);
  const digest = require('node:crypto').createHash('sha256').update(contents).digest('hex');
  dataagent.requireUnchangedAnchor(digest, contents);
  for (const changed of ['', `${contents}\n`, contents.replace('1700000', '2000000')]) {
    assert.throws(() => dataagent.requireUnchangedAnchor(digest, changed), /anchor changed/);
  }
  let now = 2000000;
  const clock = dataagent.phaseBudget(kql.ENTRY, () => now);
  now += 1000;
  assert.equal(clock.remaining(), 1139000);
  now -= 1;
  assert.throws(() => clock.remaining(), /clock moved backwards/);
  assert.equal(clock.deadline, 3140000);
  const runner = read('rust/scripts/integration-dataagent-smoke.cjs');
  assert.ok(runner.indexOf('report.enclosing_job = estimatedAnchorAdmission(') <
    runner.indexOf("const actual = git(['rev-parse', 'HEAD'])"));
  assert.ok((runner.match(/verifyAnchor\(\)/g) || []).length >= 3);
});

test('shared frozen command assembly preserves every original descriptor and rejects oversized KQL work', () => {
  const input = fixture();
  const selection = plan({...input, files: kql.ENTRY.inputs.filter(file =>
    /v2_link_kql_host\.rs$/.test(file))});
  const selected = dataagent.selectedCommands(selection, undefined, input.graph, kql.ENTRY.leaves);
  for (const entry of [kql.ENTRY, dataagentLink.ENTRY]) {
    const actual = dataagent.proofCommands(entry, selected);
    const expected = [
      {args: notebook.COMMANDS[0]},
      ...entry.leaves.map(entry.leafCommand || dataagent.leafCommand),
      ...(entry.commands || []),
      ...selected.commands,
      update.COMMANDS[4],
      ...ocv.COMMANDS.slice(3, 8),
      ...['check', 'clippy'].map(kind => ({
        args: [kind, '--offline', '--locked', '-p', 'fabric-cli', '--all-targets',
          '--no-default-features', '--features', selected.cli_features.join(','),
          ...(kind === 'clippy' ? ['--', '-D', 'warnings'] : [])],
      })),
    ];
    assert.deepEqual(actual, expected);
    assert.equal(actual.filter(command => command.args[0] === 'fetch').length, 1);
  }
  const count = dataagent.proofCommands(kql.ENTRY, selected).length;
  const extra = Array.from({length: 64 - count}, () => ({args: ['check']}));
  const atCap = {...kql.ENTRY, commands: [...kql.ENTRY.commands, ...extra]};
  assert.equal(dataagent.proofCommands(atCap, selected).length, 64);
  assert.throws(() => dataagent.proofCommands({...atCap,
    commands: [...atCap.commands, {args: ['check']}]}, selected), /inventory exceeds/);
});

test('complete KQL and preserved DataAgent leaves require their current registered cases, including headless process tests', () => {
  for (const name of kql.ENTRY.leaves) {
    const command = kql.ENTRY.leafCommand(name);
    const file = `rust/clients/cli/app/tests/${name}.rs`;
    assert.deepEqual(command.required, update.registeredTests(file));
    assert.ok(command.exhaustive && features(command).includes('headless'));
    assert.equal(features(command).includes('v2-link-kql'), name === kql.KQL_TARGET);
    const stdout = output(command.required);
    const stderr = `Running tests/${name}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, command.required.length);
    for (const wrong of ['', output([]), stdout.replace('0 ignored', '1 ignored'),
      stdout.replace('0 filtered', '1 filtered'), stdout.replace(command.required[0], 'other')]) {
      assert.throws(() => dataagent.testResults(wrong, stderr, command));
    }
    if (name === kql.KQL_TARGET) {
      const source = read(file);
      assert.match(source, /#!\[cfg\(feature = "v2-link-kql"\)\]/);
      const processNames = [...source.matchAll(
        /#\[cfg\(feature = "headless"\)\]\s*#\[test\]\s*fn\s+(\w+)/g,
      )].map(match => match[1]);
      assert.ok(processNames.length > 0, 'The registered headless process case is required');
      for (const processName of processNames) {
        assert.ok(command.required.includes(processName));
        assert.throws(() => dataagent.testResults(stdout.replace(processName, 'other'), stderr, command));
      }
    }
  }
  assert.deepEqual(features(dataagent.leafCommand(kql.KQL_TARGET)), ['headless'],
    'The existing runner default is unchanged; only the KQL entry overrides it');
});

test('KQL native-adapter and current DataAgent main namespaces use dynamic qualified inventories', () => {
  for (const module of ['v2_link_kql_main', 'v2_link_dataagent_main']) {
    const commands = kql.ENTRY.commands.filter(item => item.args.includes(`${module}::tests::`));
    assert.deepEqual(commands.map(features), [
      ['headless', 'v2-link-kql'], kql.REGISTRATION_PROFILES.at(-1).features,
    ]);
    for (const command of commands) {
      assert.ok(command.exhaustive);
      assert.deepEqual(command.required, update.registeredTests(`rust/clients/cli/app/src/${module}_tests.rs`)
        .map(name => `${module}::tests::${name}`));
      const stdout = output(command.required, 17);
      const stderr = 'Running unittests src/v2_main.rs (target/debug/deps/test.exe)\n';
      const result = dataagent.testResults(stdout, stderr, command)[0];
      assert.equal(result.passed, command.required.length);
      assert.equal(result.filtered, 17, 'Unrelated binary-unit cases remain explicitly filtered');
      for (const wrong of ['', output([]), stdout.replace('0 ignored', '1 ignored'),
        stdout.replace(command.required[0], command.required[0].split('::').at(-1))]) {
        assert.throws(() => dataagent.testResults(wrong, stderr, command));
      }
      assert.throws(() => dataagent.testResults(stdout, stderr.replace('v2_main', 'main'), command));
    }
  }
});

test('combined refresh fixtures and repaired output/preflight/help cases remain actual proof obligations', () => {
  const command = kql.ENTRY.commands.find(item => item.args.includes('v2_refresh_main::tests::'));
  assert.ok(command?.exhaustive);
  assert.deepEqual(features(command), kql.REGISTRATION_PROFILES.at(-1).features);
  assert.deepEqual(command.required,
    update.registeredTests('rust/clients/cli/app/src/v2_refresh_main_tests.rs')
      .map(name => `v2_refresh_main::tests::${name}`));
  const main = kql.mainCommand('v2_link_kql_main');
  for (const name of [
    'stalled_stderr_delivery_retains_accepted_handle_with_bounded_return',
    'owned_stderr_worker_cannot_prevent_supervised_process_exit_after_deadline',
  ]) assert.ok(main.required.includes(`v2_link_kql_main::tests::${name}`));
  assert.ok(kql.leafCommand(kql.KQL_TARGET).required
    .includes('native_invalid_shortcut_names_reject_before_open_stdin_or_authentication'));
  const entry = read('rust/clients/cli/app/src/v2_main.rs');
  assert.match(entry, /ExitCode::from\(v2_link_kql_main::run\(args\)\)/);
  assert.match(read('rust/clients/cli/app/src/v2_link_kql_main.rs'), /owned_output::owned_stream/);
  assert.match(read('rust/clients/cli/app/src/v2_livy_list_main.rs'), /super::KQL_LINK_HELP/);
});

test('shared mutation, wire and SDK LINK suites require every currently registered source test', () => {
  for (const target of kql.CONTRACT_TARGETS) {
    const command = kql.ENTRY.commands.find(item => item.args.includes(target.package) &&
      item.args.includes(target.target));
    assert.ok(command?.exhaustive);
    assert.deepEqual(command.required, update.registeredTests(target.source));
    assert.ok(kql.ENTRY.inputs.includes(target.source));
    const stdout = output(command.required);
    const stderr = `Running tests/${target.target}.rs (target/debug/deps/test.exe)\n`;
    assert.equal(dataagent.testResults(stdout, stderr, command)[0].passed, command.required.length);
    for (const wrong of ['', output([]), stdout.replace(command.required[0], 'other'),
      stdout.replace('0 ignored', '1 ignored'), stdout.replace('0 filtered', '1 filtered')]) {
      assert.throws(() => dataagent.testResults(wrong, stderr, command));
    }
  }
});

test('default, headless, registration-only and combined refresh profiles get actual check and strict Clippy', () => {
  const commands = kql.registrationCommands();
  assert.equal(commands.length, 2 * kql.REGISTRATION_PROFILES.length);
  assert.deepEqual(kql.REGISTRATION_PROFILES.map(item => item.features), [
    [], ['headless'], ['v2-link-kql'], ['headless', 'runtime-v2', 'v2-link-kql', 'v2-refresh'],
  ]);
  for (const profile of kql.REGISTRATION_PROFILES) {
    const selected = commands.filter(command => JSON.stringify(features(command)) === JSON.stringify(profile.features));
    assert.deepEqual(selected.map(command => command.args[0]), ['check', 'clippy']);
    for (const {args} of selected) {
      for (const flag of ['--offline', '--locked', '--all-targets', '--no-default-features']) {
        assert.ok(args.includes(flag));
      }
      assert.ok(!args.includes('--all-features') && !args.includes('--workspace'));
      if (args[0] === 'clippy') assert.deepEqual(args.slice(-3), ['--', '-D', 'warnings']);
    }
  }
  const component = graph().components.find(item => item.id === 'cli-kql-link');
  for (const id of ['v2-link-kql', 'v2-link-kql-headless', 'v2-link-kql-refresh-headless']) {
    assert.ok(component.profiles.includes(id));
    const profile = graph().profiles.find(item => item.id === id);
    assert.ok(profile.features.includes('fabric-cli/v2-link-kql'));
  }
  const combined = graph().profiles.find(item => item.id === 'v2-link-kql-refresh-headless');
  assert.deepEqual(combined.features.map(item => item.slice('fabric-cli/'.length)).sort(),
    kql.REGISTRATION_PROFILES.at(-1).features);
});

// TEST ONLY: synthetic metadata for four real package/target interfaces. Hosted
// proof instead captures Cargo's locked metadata for the complete workspace.
function fixture() {
  const catalog = graph();
  const ids = ['sdk-items', 'sdk-items-link', 'mutations', 'mutations-link', 'sdk',
    'cli-dataagent-list', 'cli-dataagent-link', 'cli-kql-link'];
  const components = catalog.components.filter(item => ids.includes(item.id)).map(item => ({
    ...item, depends_on: (item.depends_on || []).filter(id => ids.includes(id)), checks: [],
  }));
  assert.equal(components.length, ids.length);
  const locations = [
    ['fabric-sdk-items', 'rust/engine/control-plane/resources/items'],
    ['fabric-cli-mutations', 'rust/clients/cli/commands/mutations'],
    ['fabric-sdk', 'rust/engine/sdk'],
    ['fabric-cli', 'rust/clients/cli/app'],
  ];
  const packages = locations.map(([name, directory]) => {
    const manifest = read(`${directory}/Cargo.toml`);
    const features = Object.fromEntries([...((manifest.split('[features]')[1] || '').split(/^\[/m)[0])
      .matchAll(/^([\w-]+)\s*=/gm)].map(match => [match[1], []]));
    const targets = [...new Set(components.flatMap(item => item.tests)
      .filter(item => item.package === name).map(item => item.target))].map(target => {
      const [kind, targetName] = target.split(':');
      const section = manifest.split(`[[${kind}]]`).find(part =>
        part.match(new RegExp(`^name = "${targetName}"$`, 'm')));
      const source = kind === 'lib' ? 'src/lib.rs' :
        section?.match(/^path = "([^"]+)"$/m)?.[1] || `tests/${targetName}.rs`;
      assert.ok(fs.existsSync(path.join(ROOT, directory, source)));
      return {name: targetName || name, kind: [kind], test: true,
        src_path: path.join(ROOT, directory, source),
        'required-features': JSON.parse(section?.match(/^required-features = (\[[^\n]+\])$/m)?.[1] || '[]')};
    });
    const dependencies = locations.filter(([dependency]) => dependency !== name &&
      new RegExp(`^${dependency}(?:\\.workspace)?\\s*=`, 'm').test(manifest)).map(([dependency, location]) => ({
      name: dependency, path: path.join(ROOT, location), kind: null, optional: false, target: null,
    }));
    return {id: name, name, manifest_path: path.join(ROOT, directory, 'Cargo.toml'),
      dependencies, features, targets};
  });
  const profiles = catalog.profiles.filter(item => item.packages.every(name =>
    locations.some(([packageName]) => packageName === name)) && item.features.every(feature =>
    locations.some(([packageName]) => feature.startsWith(`${packageName}/`))))
    .map(item => ({...item, checks: []}));
  return {root: ROOT, graph: {schema: 1, components, profiles, checks: []},
    metadata: {version: 1, packages, workspace_members: packages.map(item => item.id)}};
}

test('real KQL and shared-input registrations preserve named leaves through Cargo reverse-consumer planning', () => {
  const input = fixture();
  const shared = kql.ENTRY.inputs.filter(file => /(?:v2_link(?:_wire)?|items\/tests\/links)\.rs$/.test(file));
  assert.equal(shared.length, 3);
  const kqlInputs = kql.ENTRY.inputs.filter(file =>
    /v2_link_(?:kql_(?:host|main|main_tests)|dispatch)\.rs$/.test(file));
  assert.equal(kqlInputs.length, 5);
  assert.ok(kqlInputs.includes('rust/clients/cli/app/src/v2_link_dispatch.rs'));
  for (const files of [shared, ...kqlInputs.map(file => [file])]) {
    const selection = plan({...input, files});
    assert.equal(selection.mode, 'affected');
    const selected = dataagent.selectedCommands(selection, undefined, input.graph, kql.ENTRY.leaves);
    for (const name of kql.ENTRY.leaves) {
      assert.ok(selected.commands.some(item => item.args.includes(name)), `Missing actual leaf ${name}`);
    }
    const kqlCommand = selected.commands.find(item => item.args.includes(kql.KQL_TARGET));
    assert.ok(features(kqlCommand).includes('v2-link-kql') && features(kqlCommand).includes('headless'));
    for (const [packageName, target] of [
      ['fabric-cli-mutations', 'v2_link'], ['fabric-cli-mutations', 'v2_link_wire'],
      ['fabric-sdk-items', 'links'], ['fabric-sdk', 'kql_metadata'], ['fabric-sdk', 'dataagents'],
    ]) {
      assert.ok(selection.tests.some(item => item.package === packageName && item.target === `test:${target}`));
    }
    for (const {name} of input.metadata.packages) {
      assert.ok(!selection.tests.some(item => item.package === name && item.target === 'all'));
    }
    const broken = structuredClone(input.graph);
    for (const component of broken.components) component.inputs = component.inputs?.filter(file => !files.includes(file));
    const fallback = plan({...input, graph: broken, files});
    assert.throws(() => dataagent.selectedCommands(fallback, undefined, broken, kql.ENTRY.leaves), /leaf selection/);
  }
  for (const dependency of ['mutations-link', 'sdk-items-link', 'operations', 'cli-get', 'cli-headless', 'output']) {
    assert.ok(graph().components.find(item => item.id === 'cli-kql-link').depends_on.includes(dependency));
  }
  for (const file of ['rust/clients/cli/app/src/v2_main.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main.rs',
    'rust/clients/cli/app/src/v2_link_dataagent_main_tests.rs',
    'rust/clients/cli/app/src/v2_dataagent_list_main.rs']) {
    assert.ok(kql.ENTRY.inputs.includes(file), `Missing coupled main consumer input ${file}`);
  }
});

test('an unmapped reverse consumer remains deferred rather than silently claimed as qualified', () => {
  const input = fixture();
  const name = 'proof-test-only-unmapped-consumer';
  input.metadata.packages.push({id: name, name,
    manifest_path: path.join(ROOT, 'rust', name, 'Cargo.toml'),
    features: {}, targets: [{name, kind: ['lib'], test: true,
      src_path: path.join(ROOT, 'rust', name, 'src', 'lib.rs')}],
    dependencies: [{name: 'fabric-sdk-items', path: path.join(ROOT, 'rust/engine/control-plane/resources/items'),
      kind: null, optional: false, target: null}],
  });
  input.metadata.workspace_members.push(name);
  const selection = plan({...input, files: ['rust/engine/control-plane/resources/items/tests/links.rs']});
  assert.ok(selection.packages.includes(name));
  assert.ok(selection.tests.some(item => item.package === name && item.target === 'all'));
  const selected = dataagent.selectedCommands(selection, undefined, input.graph, kql.ENTRY.leaves);
  assert.ok(selected.deferred.some(item => item.package === name && /not claimed/.test(item.disposition)));
  assert.ok(!selected.commands.some(item => item.args.includes(name)));
});

test('KQL proof keeps isolated CREATE owners separate and preserves the complete qualified Report inventory', () => {
  const targets = [
    ...kql.ENTRY.leaves.map(name => [name, name === kql.KQL_TARGET ? ['v2-link-kql'] : []]),
    ['eventhouse_create_binary', ['headless', 'v2-create-eventhouse']],
    ['kql_database_create_binary', ['headless', 'v2-kql-database-create']],
    ['create_binding_composition', ['headless', 'v2-create-eventhouse', 'v2-kql-database-create']],
    ['v2_create_report', []],
  ];
  const selection = {
    tests: targets.map(([name]) => ({package: 'fabric-cli', target: `test:${name}`,
      reasons: name === 'v2_create_report' ? ['Component closure: cli-report-create'] : []})),
    target_catalog: targets.map(([name, required]) => ({package: 'fabric-cli',
      target: `test:${name}`, source: `rust/clients/cli/app/tests/${name}.rs`, required_features: required})),
    profiles: [graph().profiles.find(item => item.id === 'v2-create-headless')],
  };
  const selected = dataagent.selectedCommands(selection, undefined, graph(), kql.ENTRY.leaves);
  const command = name => selected.commands.find(item => item.args.includes(name));
  assert.ok(!features(command('eventhouse_create_binary')).includes('v2-kql-database-create'));
  assert.ok(!features(command('kql_database_create_binary')).includes('v2-create-eventhouse'));
  for (const feature of ['v2-create-eventhouse', 'v2-kql-database-create']) {
    assert.ok(features(command('create_binding_composition')).includes(feature));
    assert.ok(selected.cli_features.includes(feature));
  }
  const report = command('v2_create_report');
  assert.ok(features(report).includes('v2-create'));
  // Isolate Report's receipt from other named targets in its feature group.
  const reportCommand = {args: ['test', '--test', 'v2_create_report']};
  const stdout = output(dataagent.REPORT_TESTS);
  const stderr = 'Running tests/v2_create_report.rs (target/debug/deps/test.exe)\n';
  assert.equal(dataagent.testResults(stdout, stderr, reportCommand)[0].passed, dataagent.REPORT_TESTS.length);
  for (const prefix of ['host::', 'host::http::', 'host::status::', 'process::']) {
    const name = dataagent.REPORT_TESTS.find(item => item.startsWith(prefix));
    assert.ok(name);
    assert.throws(() => dataagent.testResults(stdout.replace(name, 'other'), stderr, reportCommand));
  }
  assert.throws(() => dataagent.testResults(
    'test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n',
    stderr, reportCommand), /Report host\/process/);
});
