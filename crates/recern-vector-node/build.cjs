const {execFileSync} = require('node:child_process');
const {copyFileSync} = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');
execFileSync('cargo', ['build', '--locked', '--release', '-p', 'recern-vector-node'], {cwd: root, stdio:'inherit'});
const library = process.platform === 'win32' ? 'recern_vector_node.dll' : `librecern_vector_node.${process.platform === 'darwin' ? 'dylib' : 'so'}`;
copyFileSync(path.join(path.resolve(root, process.env.CARGO_TARGET_DIR || 'target'), 'release', library), path.join(__dirname, 'recern_vector.node'));
