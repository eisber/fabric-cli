// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {sourceInventory, sourceDifferences} = require('./validate-impact.cjs');

function main(argv) {
  const [output, before] = argv;
  const root = fs.realpathSync(path.resolve(__dirname, '../..'));
  if (argv.length < 1 || argv.length > 2 || !path.isAbsolute(output) ||
      (before && !path.isAbsolute(before))) {
    throw new Error('Usage: source-inventory.cjs <absolute-output> [absolute-before]');
  }
  const destination = path.resolve(output);
  if (destination === root || destination.startsWith(`${root}${path.sep}`)) {
    throw new Error('Source inventory artifacts must be outside the checkout');
  }
  const inventory = sourceInventory(root);
  fs.mkdirSync(path.dirname(destination), {recursive: true});
  fs.writeFileSync(destination, `${JSON.stringify(inventory, null, 2)}\n`, {flag: 'wx'});
  if (before) {
    const original = JSON.parse(fs.readFileSync(before, 'utf8'));
    const differences = sourceDifferences(original, inventory);
    fs.writeFileSync(`${destination}.differences.json`, `${JSON.stringify(differences, null, 2)}\n`, {flag: 'wx'});
    console.log(JSON.stringify({before: original.digest, after: inventory.digest, differences}));
    if (original.digest !== inventory.digest || differences.length) {
      throw new Error('Source inventory changed; exact per-file differences retained');
    }
  } else {
    console.log(JSON.stringify({digest: inventory.digest, files: inventory.files.length}));
  }
}

if (require.main === module) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
