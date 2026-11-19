// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {test} = require('node:test');
const {sourcePin} = require('./integration-smoke.cjs');
const target = require('./integration-target-witness.cjs');
const {registeredTests} = require('./integration-update-smoke.cjs');
const {testObservations} = require('./integration-dataagent-smoke.cjs');
const workflow = fs.readFileSync(path.resolve(__dirname, '../../.github/workflows/integration-smoke.yml'), 'utf8')
  .replace(/\r\n/g, '\n');
const block = workflow.split("      - name: Check the owned HTTP candidate's ")[1].split('\n      - name:')[0];
const inline = block.split("          @'\n")[1].split("\n          '@ | node")[0]
  .split('\n').map(line => line.slice(10)).join('\n');
const contract = inline.split('// BEGIN HTTP57 CONTRACT\n')[1].split('// END HTTP57 CONTRACT')[0];
function load(now = () => 0) {
  return new Function('assert', 'path', 'testObservations', 'deadline', 'Date',
    contract + '\nreturn {mandatory, testArgs, commandPlan, eventContract, verifyTargets, executeCommands, remaining, candidate, integration, original};')(
    assert, path, testObservations, 570000, {now});
}
const c = load();
const event = {
  number: 122,
  repository: {full_name: 'eisber/fabric-cli'},
  pull_request: {
    number: 122,
    head: {ref: c.candidate, sha: '2'.repeat(40), repo: {full_name: 'eisber/fabric-cli'}},
    base: {ref: 'dev/marcozo/fab-cli-v2-integration-20261104', sha: c.integration,
      repo: {full_name: 'eisber/fabric-cli'}},
  },
};
const names = Object.keys(c.mandatory);
const display = target => target === 'fabric_cli' ? 'unittests src/lib.rs' : `tests/${target}.rs`;
const file = target => `C:\\target\\debug\\deps\\${target}-abcdef.exe`;
const inspect = file => ({path: file, bytes: 42, sha256: 'a'.repeat(64)});
function fixture(extra = false) {
  let stdout = '', stderr = '', compile = '';
  for (const target of names) {
    const cases = [...c.mandatory[target], ...(extra && target === 'fabric_cli' ? ['additional_registered_case'] : [])];
    stderr += `Running ${display(target)} (${file(target)})\n`;
    compile += `Executable ${display(target)} (${file(target)})\n`;
    stdout += `running ${cases.length} tests\n${cases.map(name => `test ${name} ... ok\n`).join('')}` +
      `test result: ok. ${cases.length} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n`;
  }
  return {stdout, stderr, compile};
}

test('HTTP workflow route is exact-owned-repository branch and integration PR only', () => {
  const expression = block.match(/\n        if: >-\n((?:          .+\n)+)/)[1]
    .trim().replace(/\s*\n\s*/g, ' ');
  assert.equal(vm.runInNewContext(expression, {github: {event_name: 'pull_request', event}}), true);
  for (const mutate of [
    e => { e.pull_request.head.ref += '-lookalike'; },
    e => { e.pull_request.head.repo.full_name = 'untrusted/fork'; },
    e => { e.pull_request.base.repo.full_name = 'untrusted/fork'; },
    e => { e.pull_request.base.ref = 'main'; },
  ]) {
    const changed = structuredClone(event); mutate(changed);
    assert.equal(vm.runInNewContext(expression, {github: {event_name: 'pull_request', event: changed}}), false);
    assert.throws(() => c.eventContract(changed, 'pull_request'));
  }
  for (const name of ['push', 'workflow_dispatch', 'schedule']) {
    assert.equal(vm.runInNewContext(expression, {github: {event_name: name, event}}), false);
    assert.throws(() => c.eventContract(event, name));
  }
});

test('stale PR112 base event refuses and exact merge parents still use shared sourcePin', () => {
  c.eventContract(event, 'pull_request');
  for (const base of ['61c8221914152e231e5e64f7bd38418f66264999',
    'af846fe7e3ea444051aae3881491078323527d5f']) {
    const stale = structuredClone(event);
    stale.pull_request.base.sha = base;
    assert.throws(() => c.eventContract(stale, 'pull_request'));
  }
  assert.equal(c.original, '1de59573e6a683c1141213bf40b6ea3e92ed366c');
  const checkout = '3'.repeat(40), parents = [c.integration, event.pull_request.head.sha];
  const ref = 'refs/pull/122/merge';
  const runEnv = {GITHUB_EVENT_NAME: 'pull_request', GITHUB_REPOSITORY: 'eisber/fabric-cli',
    GITHUB_REF: ref, GITHUB_SHA: checkout, GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1',
    GITHUB_JOB: 'smoke', GITHUB_SERVER_URL: 'https://github.com', GITHUB_API_URL: 'https://api.github.com'};
  const witness = {schema: 1, kind: 'github-rest-exact-target-ref',
    ...target.witnessContext(event, runEnv), checkout_base: c.integration, endpoint: target.ENDPOINT,
    requested_at: '2026-01-01T00:00:00.000Z', provider_date: 'Thu, 01 Jan 2026 00:00:00 GMT',
    observed_at: '2026-01-01T00:00:00.000Z'};
  const pin = (ordered = parents, observed = witness) => sourcePin('pull_request', event, ref,
    checkout, checkout, ordered, false, observed, runEnv);
  assert.equal(pin().checkout_base, c.integration);
  assert.throws(() => pin(parents.slice().reverse()));
  assert.throws(() => pin(parents, {...witness, checkout_base: '4'.repeat(40)}),
    {code: 'TARGET_DRIFT'});
  assert.ok(inline.includes("assert.equal(report.source.checkout_base, integration,"));
  assert.ok(inline.includes("git(['merge-base', '--is-ancestor', original, event.pull_request.head.sha])"));
});

test('frozen original57 cases retain all five target groups and source registrations', () => {
  assert.deepEqual(Object.values(c.mandatory).map(value => value.length), [4, 2, 12, 18, 21]);
  assert.equal(Object.values(c.mandatory).flat().length, 57);
  for (const [target, cases] of Object.entries(c.mandatory)) {
    assert.equal(new Set(cases).size, cases.length);
    if (target !== 'fabric_cli') {
      for (const name of cases) assert.ok(registeredTests(`rust/clients/cli/app/tests/${target}.rs`).includes(name));
    }
  }
  const libSources = [
    ['folder_reads.rs', 'folder_reads::tests::'],
    ['reads/list_selection.rs', 'reads::list_selection::tests::'],
    ['tabular_export.rs', 'tabular_export::'],
  ].flatMap(([file, prefix]) => registeredTests(`rust/clients/cli/app/src/${file}`).map(name => prefix + name));
  for (const name of c.mandatory.fabric_cli) assert.ok(libSources.includes(name));
});

test('only one locked Windows fetch and exact compile/runtime pair, no feature/name filters', () => {
  const plan = c.commandPlan();
  assert.equal(plan.length, 3);
  assert.deepEqual(plan[0].args, ['fetch', '--locked', '--target', 'x86_64-pc-windows-msvc']);
  const args = ['test', '--offline', '--locked', '--jobs', '2', '-p', 'fabric-cli', '--lib',
    '--test', 'http_architecture', '--test', 'workspace_contract', '--test', 'get_options', '--test', 'get_host'];
  assert.deepEqual(plan[1].args, [...args, '--no-run']);
  assert.deepEqual(plan[2].args, args);
  assert.deepEqual(plan.map(item => item.cap), [570000, 600000, 300000]);
  assert.ok(plan.every(item => item.state === 'UNRUN'));
});

test('compile binds five executables but zero behavior credit; runtime requires original57', () => {
  const f = fixture();
  const compiled = c.verifyTargets('', f.compile, true, inspect);
  assert.equal(compiled.length, 5);
  assert.ok(compiled.every(item => !item.behavior_pass_credit));
  const runtime = c.verifyTargets(f.stdout, f.stderr, false, inspect, compiled);
  assert.equal(runtime.reduce((sum, item) => sum + item.cases.length, 0), 57);
  assert.ok(runtime.every(item => item.behavior_pass_credit && item.additional_cases.length === 0));
  assert.throws(() => c.verifyTargets(f.stdout, f.compile, true, inspect));
  assert.throws(() => c.verifyTargets(f.stdout.replace(c.mandatory.get_host[0], 'unexpected_case'),
    f.stderr, false, inspect, compiled));
  assert.throws(() => c.verifyTargets(f.stdout, f.stderr, false,
    file => ({...inspect(file), sha256: 'b'.repeat(64)}), compiled));
});

test('extra combined-source cases are reported honestly, never substituted for mandatory cases', () => {
  const f = fixture(true), compiled = c.verifyTargets('', f.compile, true, inspect);
  const runtime = c.verifyTargets(f.stdout, f.stderr, false, inspect, compiled);
  assert.equal(runtime.reduce((sum, item) => sum + item.cases.length, 0), 58);
  assert.deepEqual(runtime[0].additional_cases, ['additional_registered_case']);
});

test('failed ignored filtered orphan duplicate and wrong-stream results cannot pass', () => {
  const f = fixture(), compiled = c.verifyTargets('', f.compile, true, inspect);
  for (const stdout of [
    f.stdout.replace('... ok', '... FAILED'),
    f.stdout.replace('0 ignored', '1 ignored'), f.stdout.replace('0 filtered out', '1 filtered out'),
    f.stdout + 'test orphan ... ok\n', f.stdout.replace('running 4 tests', 'running 3 tests'),
    f.stdout.replace(`test ${c.mandatory.fabric_cli[1]}`, `test ${c.mandatory.fabric_cli[0]}`),
    f.stdout.replace('test result: ok.', 'test result: FAILED.'),
  ]) assert.throws(() => c.verifyTargets(stdout, f.stderr, false, inspect, compiled));
  assert.throws(() => c.verifyTargets(f.stdout, f.stderr + 'running 1 tests\n', false, inspect, compiled));
  assert.throws(() => c.verifyTargets(f.stdout, f.stderr.replace('tests/get_host.rs', 'tests/unrelated.rs'),
    false, inspect, compiled));
});

test('first command failure stops the actual plan loop with later commands explicitly unrun', () => {
  const plan = c.commandPlan(), calls = [];
  assert.throws(() => c.executeCommands(plan, item => {
    calls.push(item.id); return {status: 1};
  }, () => assert.fail('Failed execution must not earn oracle credit'), () => {}));
  assert.deepEqual(calls, ['windows-dependencies']);
  assert.deepEqual(plan.map(item => item.state), ['RUNNING', 'UNRUN', 'UNRUN']);
  let time = 0;
  const expired = load(() => time), bounded = expired.commandPlan();
  assert.throws(() => expired.executeCommands(bounded, () => {
    time = 570000; return {status: 0};
  }, () => {}, () => {}));
  assert.deepEqual(bounded.map(item => item.state), ['RUNNING', 'UNRUN', 'UNRUN']);
  assert.throws(() => expired.remaining());
});

test('workflow preserves global budgets ordering and source/raw/finalization evidence', () => {
  new vm.Script(inline);
  assert.match(block, /timeout-minutes: 10/);
  assert.match(workflow, /timeout-minutes: 45/);
  assert.match(block, /RUST_TEST_THREADS: "1"/);
  assert.match(inline, /const started = Date\.now\(\), deadline = started \+ 570000/);
  assert.match(inline, /timeout: Math\.min\(cap, remaining\(\)\)/);
  for (const text of ['source-before.json', 'source-after.json', 'source-differences.json',
    'requireUnchanged(before, after)', 'report.input_hashes', 'remaining_after_finalization_ms',
    'UNRUN', 'sha256', 'feature_profile', 'sourcePin(', 'report.actual_case_count']) {
    assert.ok(inline.includes(text), text);
  }
  assert.ok(workflow.indexOf("Check the PR54 candidate's") < workflow.indexOf("Check the owned HTTP candidate's"));
  assert.ok(workflow.indexOf("Check the PR52 candidate's") < workflow.indexOf("Check the owned HTTP candidate's"));
  assert.match(workflow, /rust\/scripts\/integration-http-cli57-smoke\.test\.cjs/);
  assert.ok(!block.includes('continue-on-error'));
});
