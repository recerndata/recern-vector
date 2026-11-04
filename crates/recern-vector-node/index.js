'use strict';
const {existsSync} = require('node:fs');
const path = require('node:path');
const manifest = require('./package.json');

// Source builds and the self-contained GitHub archives keep the addon here.
// The registry package uses an exact-version optional dependency for this host.
let binding;
if (existsSync(path.join(__dirname, 'recern_vector.node'))) {
  binding = require('./recern_vector.node');
} else {
  const target = {
    'darwin-arm64': 'darwin-arm64',
    'darwin-x64': 'darwin-x64',
    'linux-arm64': 'linux-arm64-gnu',
    'linux-x64': 'linux-x64-gnu',
    'win32-x64': 'win32-x64-msvc',
  }[`${process.platform}-${process.arch}`];
  if (!target) throw new Error(`Recern Vector has no prebuilt addon for ${process.platform}/${process.arch}. Build from source on other targets.`);
  try {
    binding = require(`@recern/vector-${target}`);
  } catch (cause) {
    throw new Error(`Cannot load @recern/vector-${target}@${manifest.version}. Install optional dependencies (npm install --include=optional); Linux prebuilds require glibc 2.35+.`, {cause});
  }
}
if (binding.version() !== manifest.version) {
  throw new Error(`Recern Vector addon version ${binding.version()} does not match JavaScript package ${manifest.version}. Reinstall the matching package or rebuild from source.`);
}
module.exports = binding;
