// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const assert = require('node:assert/strict');
const path = require('node:path');
const {test} = require('node:test');
const {VERSION, IGNORE, options, commands, summarize, markdown} = require('./rust-coverage.cjs');

const root = path.resolve(__dirname, '..', '..');
const output = path.join(path.parse(root).root, 'coverage-unit-artifacts');
const metrics = (count, covered) => Object.fromEntries(
  ['lines', 'functions', 'regions'].map(name => [name, {count, covered, percent: 99.99}]),
);
function fixture() {
  const directory = path.join(root, 'rust', 'sdk', 'sample');
  return {
    metadata: {
      workspace_members: ['sample'],
      packages: [{id: 'sample', name: 'sample', manifest_path: path.join(directory, 'Cargo.toml')}],
    },
    report: {type: 'llvm.coverage.json.export', data: [{files: [
      {filename: path.join(directory, 'src', 'lib.rs'), summary: metrics(10, 3)},
      {filename: path.join(directory, 'src', 'error.rs'), summary: metrics(30, 27)},
    ]}]},
  };
}

test('full coverage executes all targets and features without hiding test failures', () => {
  const opts = options(['--output', output]);
  const plan = commands(opts);
  assert.equal(VERSION, '0.9.1');
  assert.ok(plan[0].args.includes('--workspace'));
  assert.ok(plan[0].args.includes('--all-targets'));
  assert.ok(plan[0].args.includes('--all-features'));
  assert.ok(plan[0].args.includes('--no-fail-fast'));
  assert.ok(!plan[0].args.includes('--ignore-run-fail'));
  assert.deepEqual(plan.map(command => command.id), ['tests', 'json', 'lcov', 'html']);
  assert.ok(plan.every(command => command.args.includes('--locked') && command.args.includes('--offline')));
});

test('package coverage is explicitly narrowed rather than called full coverage', () => {
  const opts = options(['--output', output, '--scope', 'packages', '--packages', 'b,a']);
  const tests = commands(opts)[0];
  assert.ok(!tests.args.includes('--workspace'));
  assert.deepEqual(opts.packages, ['a', 'b']);
  assert.throws(() => options(['--output', output, '--packages', 'a']), /forbids/);
  assert.throws(() => options(['--output', output, '--scope', 'packages']), /requires/);
});

test('unsafe scopes, output paths and unbounded durations fail before execution', () => {
  assert.throws(() => options(['--output', path.join(root, 'rust', 'coverage')]), /outside/);
  assert.throws(() => options(['--output', root]), /outside/);
  assert.throws(() => options(['--output', 'relative']), /absolute/);
  assert.throws(() => options(['--output', output, '--timeout-seconds', 'Infinity']), /timeout/);
  assert.throws(() => options(['--output', output, '--timeout-seconds', '14401']), /timeout/);
  assert.throws(() => options(['--output', output, '--scope', 'packages', '--packages', 'a,a']), /duplicate/);
  assert.throws(() => options(['--output', output, '--scope', 'packages', '--packages', 'a;echo']), /Invalid/);
  assert.throws(() => options(['--output', output, '--execute', '--execute']), /Duplicate/);
  assert.throws(() => options(['--output', output, '--tool', 'relative']), /absolute/);
});

test('coverage percentages use covered/count denominators, not averages or test counts', () => {
  const {metadata, report} = fixture();
  const summary = summarize(report, metadata, root, 'full');
  assert.deepEqual(summary.totals.lines, {count: 40, covered: 30, percent: 75});
  assert.deepEqual(summary.packages[0].functions, {count: 40, covered: 30, percent: 75});
  assert.equal(summary.files.length, 2);
  assert.equal(summary.branch_coverage.measured, false);
  assert.equal(summary.doctest_coverage.measured, false);
  assert.ok(summary.denominator.includes('inline unit-test'));
  assert.deepEqual(summary.unmeasured_packages, []);
});

test('unmeasured platform/package sources are not reported as zero-covered measured lines', () => {
  const {metadata, report} = fixture();
  metadata.workspace_members.push('other');
  metadata.packages.push({id: 'other', name: 'other',
    manifest_path: path.join(root, 'rust', 'sdk', 'other', 'Cargo.toml')});
  const summary = summarize(report, metadata, root, 'packages');
  assert.deepEqual(summary.unmeasured_packages, ['other']);
  assert.equal(summary.totals.lines.count, 40);
  const text = markdown({...summary, head: 'a'.repeat(40), host: 'native-test-host',
    tests_status: 'failed'});
  assert.match(text, /Tests: \*\*failed\*\*/);
  assert.match(text, /partial diagnostic evidence/);
  assert.match(text, /30\/40 \(75\.00%\)/);
});

test('reports reject vacuous, duplicate, malformed and unowned sources', () => {
  const {metadata, report} = fixture();
  assert.throws(() => summarize({...report, data: [{files: []}]}, metadata, root, 'full'), /no measured/);
  assert.throws(() => summarize({...report, type: 'unknown'}, metadata, root, 'full'), /Unsupported/);
  const repeated = structuredClone(report);
  repeated.data[0].files.push(repeated.data[0].files[0]);
  assert.throws(() => summarize(repeated, metadata, root, 'full'), /Duplicate/);
  for (const bad of [metrics(10, 11), metrics(-1, 0), metrics(1.5, 1)]) {
    const invalid = structuredClone(report);
    invalid.data[0].files[0].summary = bad;
    assert.throws(() => summarize(invalid, metadata, root, 'full'), /Invalid/);
  }
  const external = structuredClone(report);
  external.data[0].files[0].filename = path.join(root, 'outside.rs');
  assert.throws(() => summarize(external, metadata, root, 'full'), /Unexpected/);
});

test('standalone tests, examples and generated target files are excluded on both path styles', () => {
  const pattern = new RegExp(IGNORE);
  for (const value of ['a/tests/t.rs', 'a\\tests\\t.rs', 'a/examples/demo.rs',
    'a\\target\\debug\\generated.rs']) assert.ok(pattern.test(value), value);
  for (const value of ['a/src/lib.rs', 'a\\src\\tests.rs']) assert.ok(!pattern.test(value), value);
  const {metadata, report} = fixture();
  report.data[0].files[0].filename = path.join(root, 'rust', 'sdk', 'sample', 'tests', 'test.rs');
  assert.throws(() => summarize(report, metadata, root, 'full'), /Unexpected/);
});
