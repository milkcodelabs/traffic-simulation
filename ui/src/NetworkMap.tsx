import { MapboxOverlay } from "@deck.gl/mapbox";
import { PathLayer } from "@deck.gl/layers";
import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import maplibregl, { type GeoJSONSource, type Map } from "maplibre-gl";
import { congestionColor, queueColor } from "./frame";

const WHITE_TOWER: [number, number] = [22.9484, 40.6264];

type RoadPath = {
  index: number;
  path: [number, number][];
  lanes: number;
};

export type CameraPreset = "fit" | "city" | "centre";

export type MapHandle = {
  setFields: (speed: Float32Array, density: Float32Array) => void;
  camera: (preset: CameraPreset) => void;
};

type Props = {
  center: [number, number];
  data: GeoJSON.FeatureCollection | null;
  showQueue: boolean;
};

export const NetworkMap = forwardRef<MapHandle, Props>(function NetworkMap({ center, data, showQueue }, ref) {
  const container = useRef<HTMLDivElement>(null);
  const mapRef = useRef<Map | null>(null);
  const overlayRef = useRef<MapboxOverlay | null>(null);
  const pathsRef = useRef<RoadPath[]>([]);
  const speedRef = useRef<Float32Array | null>(null);
  const densityRef = useRef<Float32Array | null>(null);
  const showQueueRef = useRef(showQueue);
  const versionRef = useRef(0);
  const dataRef = useRef(data);
  const centerRef = useRef(center);
  showQueueRef.current = showQueue;
  dataRef.current = data;
  centerRef.current = center;

  useImperativeHandle(ref, () => ({
    setFields(speed, density) {
      speedRef.current = speed;
      densityRef.current = density;
      versionRef.current += 1;
      paint(overlayRef.current, pathsRef.current, speedRef.current, densityRef.current, showQueueRef.current, versionRef.current);
    },
    camera(preset) {
      const map = mapRef.current;
      if (!map) {
        return;
      }
      if (preset === "fit") {
        fit(map, dataRef.current);
        return;
      }
      if (preset === "centre") {
        map.flyTo({ center: centreOf(dataRef.current, centerRef.current), zoom: 14.2, duration: 700 });
        return;
      }
      map.flyTo({ center: centerRef.current, zoom: 12, duration: 700 });
    },
  }));

  useEffect(() => {
    if (!container.current || mapRef.current) {
      return;
    }
    const map = new maplibregl.Map({
      container: container.current,
      style: "https://tiles.openfreemap.org/styles/dark",
      center,
      zoom: 12,
    });
    map.addControl(new maplibregl.NavigationControl({ visualizePitch: false }), "top-right");
    const overlay = new MapboxOverlay({ interleaved: false, layers: [] });
    map.addControl(overlay as unknown as maplibregl.IControl);
    overlayRef.current = overlay;
    map.on("load", () => {
      map.addSource("network", {
        type: "geojson",
        data: { type: "FeatureCollection", features: [] },
      });
      map.addLayer({
        id: "network-casing",
        type: "line",
        source: "network",
        paint: {
          "line-color": "#141812",
          "line-width": ["interpolate", ["linear"], ["zoom"], 11, 2.2, 15, 7],
          "line-opacity": 0.9,
        },
      });
      if (dataRef.current) {
        publishRoads(map, overlay, dataRef.current, pathsRef, versionRef, speedRef.current, densityRef.current, showQueueRef.current);
      }
    });
    mapRef.current = map;
    return () => {
      overlay.finalize();
      map.remove();
      mapRef.current = null;
      overlayRef.current = null;
    };
    // The map is created once. Later datasets and frames go through the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const map = mapRef.current;
    const overlay = overlayRef.current;
    if (!map || !overlay || !data) {
      return;
    }
    const apply = () => publishRoads(map, overlay, data, pathsRef, versionRef, speedRef.current, densityRef.current, showQueueRef.current);
    if (map.isStyleLoaded()) {
      apply();
    } else {
      map.once("load", apply);
    }
  }, [data]);

  useEffect(() => {
    versionRef.current += 1;
    paint(overlayRef.current, pathsRef.current, speedRef.current, densityRef.current, showQueue, versionRef.current);
  }, [showQueue]);

  return <div ref={container} className="absolute inset-0 h-full w-full" style={{ position: "absolute", inset: 0 }} />;
});

function publishRoads(
  map: Map,
  overlay: MapboxOverlay,
  data: GeoJSON.FeatureCollection,
  pathsRef: { current: RoadPath[] },
  versionRef: { current: number },
  speed: Float32Array | null,
  density: Float32Array | null,
  showQueue: boolean,
) {
  const source = map.getSource("network") as GeoJSONSource | undefined;
  source?.setData(data);
  pathsRef.current = roadPaths(data);
  versionRef.current += 1;
  paint(overlay, pathsRef.current, speed, density, showQueue, versionRef.current);
  fit(map, data);
}

function roadPaths(data: GeoJSON.FeatureCollection): RoadPath[] {
  const paths: RoadPath[] = [];
  data.features.forEach((feature, index) => {
    if (feature.geometry.type !== "LineString") {
      return;
    }
    const lanes = typeof feature.properties?.lanes === "number" ? feature.properties.lanes : 1;
    paths.push({
      index,
      path: feature.geometry.coordinates as [number, number][],
      lanes,
    });
  });
  return paths;
}

function paint(
  overlay: MapboxOverlay | null,
  paths: RoadPath[],
  speed: Float32Array | null,
  density: Float32Array | null,
  showQueue: boolean,
  version: number,
) {
  if (!overlay) {
    return;
  }
  overlay.setProps({
    layers: [
      new PathLayer<RoadPath>({
        id: "queues",
        data: paths,
        visible: showQueue && paths.length > 0,
        pickable: false,
        getPath: (road) => road.path,
        getColor: (road) => queueColor(density?.[road.index] ?? 0),
        getWidth: (road) => 3 + 16 * (density?.[road.index] ?? 0),
        widthUnits: "pixels",
        widthMinPixels: 1,
        capRounded: true,
        jointRounded: true,
        parameters: { depthCompare: "always", depthWriteEnabled: false },
        updateTriggers: { getColor: version, getWidth: version },
      }),
      new PathLayer<RoadPath>({
        id: "congestion",
        data: paths,
        visible: paths.length > 0,
        pickable: false,
        getPath: (road) => road.path,
        getColor: (road) => congestionColor(speed?.[road.index] ?? 1),
        getWidth: (road) => 1.4 + road.lanes * 0.9,
        widthUnits: "pixels",
        widthMinPixels: 1,
        capRounded: true,
        jointRounded: true,
        parameters: { depthCompare: "always", depthWriteEnabled: false },
        updateTriggers: { getColor: version },
      }),
    ],
  });
}

function fit(map: Map, data: GeoJSON.FeatureCollection | null) {
  const box = extent(data);
  if (!box) {
    return;
  }
  map.fitBounds(
    [
      [box.minLon, box.minLat],
      [box.maxLon, box.maxLat],
    ],
    { padding: { left: 390, right: 340, top: 56, bottom: 92 }, duration: 700 },
  );
}

function centreOf(data: GeoJSON.FeatureCollection | null, fallback: [number, number]): [number, number] {
  const box = extent(data);
  if (box && WHITE_TOWER[0] >= box.minLon && WHITE_TOWER[0] <= box.maxLon && WHITE_TOWER[1] >= box.minLat && WHITE_TOWER[1] <= box.maxLat) {
    return WHITE_TOWER;
  }
  return fallback;
}

function extent(data: GeoJSON.FeatureCollection | null) {
  if (!data) {
    return null;
  }
  let minLon = Infinity;
  let minLat = Infinity;
  let maxLon = -Infinity;
  let maxLat = -Infinity;
  for (const feature of data.features) {
    if (feature.geometry.type !== "LineString") {
      continue;
    }
    for (const [lon, lat] of feature.geometry.coordinates) {
      minLon = Math.min(minLon, lon);
      maxLon = Math.max(maxLon, lon);
      minLat = Math.min(minLat, lat);
      maxLat = Math.max(maxLat, lat);
    }
  }
  if (!Number.isFinite(minLon)) {
    return null;
  }
  return { minLon, minLat, maxLon, maxLat };
}
