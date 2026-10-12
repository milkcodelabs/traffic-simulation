// Unpack a traffic frame the same way ui/src/frame.ts does, and time it.
// Column views share the underlying buffer. A second loop copies them into new typed arrays.

import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

const file = process.argv[2];
if (!file) {
  console.error("usage: node tools/bench-frame-decode.mjs <frame.bin>");
  process.exit(1);
}

const bytes = readFileSync(file);
const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
const HEADER_BYTES = 80;

function decodeFrame(source) {
  const view = new DataView(source);
  if (source.byteLength < HEADER_BYTES) {
    throw new Error("simulation frame is shorter than its header");
  }
  const magic = view.getUint32(0, true);
  if (magic !== 0x54534631) {
    throw new Error("simulation frame is not a traffic frame");
  }
  const version = view.getUint32(4, true);
  if (version !== 1) {
    throw new Error(`simulation frame version ${version} is not 1`);
  }
  const flags = view.getUint32(28, true);
  const edges = view.getUint32(72, true);
  const listed = view.getUint32(76, true);
  const bottlenecks = [];
  let offset = HEADER_BYTES;
  for (let index = 0; index < listed; index += 1) {
    bottlenecks.push({
      edge: view.getUint32(offset, true),
      speedRatio: view.getFloat32(offset + 4, true),
      queueVeh: view.getFloat32(offset + 8, true),
    });
    offset += 12;
  }
  const speedRatio = new Float32Array(source, offset, edges);
  offset += edges * 4;
  const densityRatio = new Float32Array(source, offset, edges);
  offset += edges * 4;
  const queueVeh = new Float32Array(source, offset, edges);
  return {
    header: {
      sequence: Number(view.getBigUint64(8, true)),
      timeS: view.getFloat64(16, true),
      speed: view.getFloat32(24, true),
      paused: (flags & 1) !== 0,
      seeking: (flags & 2) !== 0,
      meanSpeedMps: view.getFloat32(32, true),
      vhtS: view.getFloat64(36, true),
      vktM: view.getFloat64(44, true),
      delayS: view.getFloat64(52, true),
      spawned: view.getUint32(60, true),
      active: view.getUint32(64, true),
      completed: view.getUint32(68, true),
    },
    unchanged: (flags & 4) !== 0,
    bottlenecks,
    speedRatio,
    densityRatio,
    queueVeh,
  };
}

const warmup = decodeFrame(buffer);
let sink = warmup.header.active + warmup.speedRatio.length + warmup.bottlenecks.length;
const iterations = 200;
const viewStart = performance.now();
for (let index = 0; index < iterations; index += 1) {
  const decoded = decodeFrame(buffer);
  sink += decoded.header.active + decoded.speedRatio[0] + decoded.densityRatio[1] + decoded.queueVeh[2];
  sink += decoded.bottlenecks.length;
}
const viewMs = performance.now() - viewStart;

const copyStart = performance.now();
for (let index = 0; index < iterations; index += 1) {
  const decoded = decodeFrame(buffer);
  const speed = new Float32Array(decoded.speedRatio);
  const density = new Float32Array(decoded.densityRatio);
  const queue = new Float32Array(decoded.queueVeh);
  sink += speed[0] + density[0] + queue[0];
}
const copyMs = performance.now() - copyStart;

console.log(
  JSON.stringify({
    bytes: bytes.length,
    edges: warmup.speedRatio.length,
    iterations,
    view_mean_us: (viewMs / iterations) * 1000,
    copy_mean_us: (copyMs / iterations) * 1000,
    sink,
  }),
);
