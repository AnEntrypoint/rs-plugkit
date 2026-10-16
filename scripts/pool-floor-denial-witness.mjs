// Usage: node scripts/pool-floor-denial-witness.mjs (from the rs-plugkit root).
// Checks that the pool-floor-met denial text built in transitions.rs carries the
// same instruction sentences as the pool-floor-met gate message in lean_graph.json.
// Prints PASS or FAIL with both segments; exits 1 on FAIL.

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const orchestrator = path.join(root, 'crates', 'plugkit-core', 'src', 'orchestrator');
const transitionsPath = path.join(orchestrator, 'transitions.rs');
const graphPath = path.join(orchestrator, 'lean_graph.json');

const START = 'Call pool-observe';
const END = 'otherwise re-attempt';

function segment(text, label) {
  const from = text.indexOf(START);
  if (from < 0) throw new Error(`${label}: no "${START}" in text`);
  const to = text.indexOf(END, from);
  if (to < 0) throw new Error(`${label}: no "${END}" after "${START}"`);
  return text.slice(from, to + END.length);
}

function rustDenialLiteral(source) {
  const anchor = '"pool-floor-met denied:';
  const from = source.indexOf(anchor);
  if (from < 0) throw new Error('transitions.rs: pool-floor-met denial literal not found');
  const end = source.indexOf('",', from);
  if (end < 0) throw new Error('transitions.rs: denial literal is not terminated');
  return source.slice(from + 1, end);
}

const failures = [];
const transitions = fs.readFileSync(transitionsPath, 'utf8');
const graph = JSON.parse(fs.readFileSync(graphPath, 'utf8'));

const gate = (graph.gates || []).find((g) => g.name === 'pool-floor-met');
if (!gate) failures.push('lean_graph.json: no gate named pool-floor-met');
if (!transitions.includes('Some("pool-floor-met") => Some(pool_floor_denial_detail())')) {
  failures.push('transitions.rs: pool-floor-met is not mapped to pool_floor_denial_detail()');
}

const rustText = gate ? rustDenialLiteral(transitions) : '';
const rustSegment = gate ? segment(rustText, 'transitions.rs') : '';
const graphSegment = gate ? segment(gate.message, 'lean_graph.json') : '';
if (gate && rustSegment !== graphSegment) {
  failures.push('denial text differs between transitions.rs and lean_graph.json');
}

if (failures.length === 0) {
  console.log(`transitions.rs segment: ${rustSegment}`);
  console.log(`lean_graph.json segment: ${graphSegment}`);
  console.log('RESULT: PASS pool-floor-met denial text matches lean_graph.json');
  process.exitCode = 0;
} else {
  for (const f of failures) console.log(`FAIL ${f}`);
  console.log(`transitions.rs segment: ${rustSegment}`);
  console.log(`lean_graph.json segment: ${graphSegment}`);
  console.log('RESULT: FAIL');
  process.exitCode = 1;
}
