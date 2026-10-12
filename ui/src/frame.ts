export type FrameHeader = {
  sequence: number;
  timeS: number;
  speed: number;
  paused: boolean;
  seeking: boolean;
  meanSpeedMps: number;
  vhtS: number;
  vktM: number;
  delayS: number;
  spawned: number;
  active: number;
  completed: number;
};

export type Bottleneck = {
  edge: number;
  speedRatio: number;
  queueVeh: number;
};

export type DecodedFrame = {
  header: FrameHeader;
  unchanged: boolean;
  bottlenecks: Bottleneck[];
  speedRatio: Float32Array;
  densityRatio: Float32Array;
  queueVeh: Float32Array;
};

const HEADER_BYTES = 80;

export function decodeFrame(buffer: ArrayBuffer): DecodedFrame {
  const view = new DataView(buffer);
  if (buffer.byteLength < HEADER_BYTES) {
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
  const bottlenecks: Bottleneck[] = [];
  let offset = HEADER_BYTES;
  for (let index = 0; index < listed; index += 1) {
    bottlenecks.push({
      edge: view.getUint32(offset, true),
      speedRatio: view.getFloat32(offset + 4, true),
      queueVeh: view.getFloat32(offset + 8, true),
    });
    offset += 12;
  }
  const speedRatio = column(buffer, offset, edges);
  offset += edges * 4;
  const densityRatio = column(buffer, offset, edges);
  offset += edges * 4;
  const queueVeh = column(buffer, offset, edges);
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

function column(buffer: ArrayBuffer, offset: number, edges: number) {
  return new Float32Array(buffer, offset, edges);
}

export function congestionColor(ratio: number): [number, number, number, number] {
  if (ratio >= 0.8) {
    return [61, 190, 110, 230];
  }
  if (ratio >= 0.5) {
    return [226, 162, 58, 235];
  }
  if (ratio >= 0.25) {
    return [210, 75, 58, 240];
  }
  return [110, 30, 51, 245];
}

export function queueColor(density: number): [number, number, number, number] {
  if (density <= 0.02) {
    return [0, 0, 0, 0];
  }
  const alpha = Math.round(50 + 170 * Math.min(1, density));
  if (density < 0.35) {
    return [226, 162, 58, alpha];
  }
  if (density < 0.7) {
    return [210, 75, 58, alpha];
  }
  return [110, 30, 51, Math.min(255, alpha + 20)];
}

export function clockLabel(seconds: number) {
  const wrapped = ((Math.floor(seconds) % 86400) + 86400) % 86400;
  const hours = Math.floor(wrapped / 3600);
  const minutes = Math.floor((wrapped % 3600) / 60);
  const secs = wrapped % 60;
  return [hours, minutes, secs].map((part) => String(part).padStart(2, "0")).join(":");
}
