export type BBox = {
  minLat: number;
  minLon: number;
  maxLat: number;
  maxLon: number;
};

export type Preset = {
  id: string;
  label: string;
  bbox: BBox;
};

export type NetworkStats = {
  nodes: number;
  edges: number;
  turns: number;
  bannedTurns: number;
  signals: number;
  gateways: number;
  zones: number;
  totalLengthM: number;
  totalLaneM: number;
  meanEdgeLengthM: number;
  contractions: number;
  bbox: BBox;
  diagram: string;
};

export type IngestReport = {
  cachePath: string;
  rawPath: string | null;
  stats: NetworkStats;
  warnings: string[];
};

export type IngestRequest =
  | { kind: "preset"; name: string; refresh: boolean }
  | { kind: "bbox"; bbox: BBox; refresh: boolean }
  | { kind: "file"; path: string };

const CLASS_NAMES = [
  "motorway",
  "motorway_link",
  "trunk",
  "trunk_link",
  "primary",
  "primary_link",
  "secondary",
  "secondary_link",
  "tertiary",
  "tertiary_link",
  "unclassified",
  "residential",
  "living_street",
  "service",
] as const;

export type NetworkFeature = GeoJSON.Feature<GeoJSON.LineString, { className: string; lanes: number; speedKph: number }>;

export function decodeViewport(buffer: ArrayBuffer): GeoJSON.FeatureCollection<GeoJSON.LineString, NetworkFeature["properties"]> {
  const view = new DataView(buffer);
  const magic = view.getUint32(0, true);
  if (magic !== 0x54534e31) {
    throw new Error("viewport blob is not a traffic network");
  }
  const edgeCount = view.getUint32(4, true);
  const features: NetworkFeature[] = [];
  let offset = 8;
  for (let edge = 0; edge < edgeCount; edge += 1) {
    const classId = view.getUint8(offset + 4);
    const lanes = view.getUint8(offset + 5);
    const speedKph = view.getUint16(offset + 6, true);
    const pointCount = view.getUint16(offset + 8, true);
    offset += 10;
    const coordinates: [number, number][] = [];
    for (let point = 0; point < pointCount; point += 1) {
      const lon = view.getFloat32(offset, true);
      const lat = view.getFloat32(offset + 4, true);
      coordinates.push([lon, lat]);
      offset += 8;
    }
    features.push({
      type: "Feature",
      geometry: { type: "LineString", coordinates },
      properties: {
        className: CLASS_NAMES[classId] ?? "unclassified",
        lanes,
        speedKph,
      },
    });
  }
  return { type: "FeatureCollection", features };
}
