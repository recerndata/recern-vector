// A JavaScript envelope may use native JSON as long as inner portable JSON
// remains text. This is a codec boundary check, not a Node remote-client test.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const fixture = JSON.parse(readFileSync(new URL("../fixtures/http-v1.json", import.meta.url), "utf8"));
const encoded = JSON.stringify(fixture);
const again = JSON.parse(encoded);
assert.deepEqual(again, fixture);
const record = again.cases.find(c => c.name === "upsert").request.records[0];
assert.equal(typeof record.metadata_json, "string");
assert.match(record.metadata_json, /18446744073709551615/);
assert.match(record.metadata_json, /\[1,1\.0\]/);
assert.equal(again.cases.find(c => c.name === "uint64-count").response.count, "18446744073709551615");
const id = "line\n/東京\u0000";
assert.equal(JSON.parse(JSON.stringify({ id })).id, id);

// Native UTF-16 sort puts this supplementary character before U+E000. The
// portable order is Unicode scalar order, matching Python and Rust strings.
const ids = ["\u{1f600}", "\ue000", "doc/東京", ""];
function compareScalars(a, b) {
  const left = Array.from(a, c => c.codePointAt(0));
  const right = Array.from(b, c => c.codePointAt(0));
  for (let i = 0; i < Math.min(left.length, right.length); i++) {
    if (left[i] !== right[i]) return left[i] - right[i];
  }
  return left.length - right.length;
}
assert.deepEqual(ids.sort(compareScalars), ["", "doc/東京", "\ue000", "\u{1f600}"]);
console.log("HTTP v1 JavaScript envelope: uint64 text, number kinds, Unicode IDs and ordering OK");
