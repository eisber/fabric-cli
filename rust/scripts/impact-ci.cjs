// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');

function selection(plan) {
  const sorted = items => [...items].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
  const tests = items => sorted(items.map(test => ({package: test.package, target: test.target,
    ...(test.filter ? {filter: test.filter} : {}),
    default_available: test.default_available !== false && test.default !== false &&
      !test.default_skipped && !test.profile_only,
    default_skipped_targets: sorted(test.default_skipped_targets || []),
    profiles: sorted(test.profiles || []),
  })));
  return {
    mode: plan.mode,
    files: sorted(plan.files),
    packages: sorted(plan.packages),
    tests: tests(plan.tests),
    profiles: sorted(plan.profiles.map(profile => ({id: profile.id,
      packages: sorted(profile.packages), features: sorted(profile.features),
      platforms: sorted(profile.platforms), checks: sorted(profile.checks || []),
      tests: tests(profile.tests || plan.tests.filter(test => profile.packages.includes(test.package))),
    }))),
    checks: sorted(plan.checks),
  };
}

function equalSelection(expected, actual) {
  if (JSON.stringify(selection(expected)) !== JSON.stringify(selection(actual))) {
    throw new Error('Native impact selection differs from the recorded selection job');
  }
}

function main(argv) {
  const [mode, directory] = argv;
  if (!['select', 'native'].includes(mode) || !directory || !path.isAbsolute(directory) || argv.length !== 2) {
    throw new Error('Usage: impact-ci.cjs select|native <absolute-artifact-directory>');
  }
  const root = path.resolve(__dirname, '..', '..');
  const head = execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root, encoding: 'utf8'}).trim();
  const requestFile = path.join(directory, 'request.json');
  const selectedFile = path.join(directory, 'selected-plan.json');
  fs.mkdirSync(directory, {recursive: true});
  const request = mode === 'select' ? {
    schema: 1, head, base: process.env.IMPACT_BASE || null,
    full: ['workflow_dispatch', 'schedule'].includes(process.env.GITHUB_EVENT_NAME),
  } : JSON.parse(fs.readFileSync(requestFile, 'utf8'));
  if (request.schema !== 1 || request.head !== head ||
      typeof request.full !== 'boolean' || (request.base !== null && typeof request.base !== 'string')) {
    throw new Error('Selection request does not match the checked-out head');
  }
  const output = mode === 'select' ? selectedFile : path.join(directory, 'native-plan.json');
  execFileSync(process.execPath, [path.join(__dirname, 'impact-plan.cjs'),
    '--root', root, '--head', head,
    ...(request.full ? ['--full'] : request.base ? ['--base', request.base] : ['--full']),
    '--output', output], {cwd: root, stdio: 'inherit', timeout: 120000});
  const plan = JSON.parse(fs.readFileSync(output, 'utf8'));
  if (mode === 'native') {
    equalSelection(JSON.parse(fs.readFileSync(selectedFile, 'utf8')), plan);
  } else {
    fs.writeFileSync(requestFile, `${JSON.stringify(request, null, 2)}\n`);
    const native = plan.packages.length > 0 || plan.checks.length > 0;
    if (process.env.GITHUB_OUTPUT) {
      fs.appendFileSync(process.env.GITHUB_OUTPUT, `run_native=${native}\n`);
    }
    if (process.env.GITHUB_STEP_SUMMARY) {
      fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
        `## Rust dependency impact\n\nHead: \`${head}\`\n\n` +
        `Base: \`${request.base || 'full validation'}\`\n\n` +
        `Mode: **${plan.mode}**\n\n` +
        `Packages: ${plan.packages.map(name => `\`${name}\``).join(', ') || 'none'}\n\n` +
        `${plan.tests.length} target selections; ${plan.profiles.length} feature profiles; ` +
        `${plan.checks.length} contract checks. The uploaded plan records reasons and inputs.\n`);
    }
  }
}

module.exports = {selection, equalSelection};
if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
