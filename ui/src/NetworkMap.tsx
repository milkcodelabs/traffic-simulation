import { useEffect, useRef } from "react";
import maplibregl, { type GeoJSONSource, type Map } from "maplibre-gl";

const CLASS_COLOR: Record<string, string> = {
  motorway: "#e2b15a",
  motorway_link: "#c99645",
  trunk: "#d38b4a",
  trunk_link: "#b8743d",
  primary: "#d36a45",
  primary_link: "#b35a3b",
  secondary: "#c47d73",
  secondary_link: "#a86a62",
  tertiary: "#8ea4b8",
  tertiary_link: "#73889a",
  unclassified: "#8d98a3",
  residential: "#6f7c86",
  living_street: "#5e6b62",
  service: "#4e5854",
};

type Props = {
  center: [number, number];
  data: GeoJSON.FeatureCollection | null;
};

export function NetworkMap({ center, data }: Props) {
  const container = useRef<HTMLDivElement>(null);
  const mapRef = useRef<Map | null>(null);

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
          "line-width": ["interpolate", ["linear"], ["zoom"], 11, 1.2, 15, 5],
        },
      });
      map.addLayer({
        id: "network-roads",
        type: "line",
        source: "network",
        paint: {
          "line-color": classColorExpression(),
          "line-width": ["interpolate", ["linear"], ["zoom"], 11, ["*", ["get", "lanes"], 0.7], 15, ["*", ["get", "lanes"], 1.8]],
          "line-opacity": 0.92,
        },
      });
      if (data) {
        publish(map, data);
      }
    });
    mapRef.current = map;
    return () => {
      map.remove();
      mapRef.current = null;
    };
    // The map is created once. Later datasets go through the effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !data) {
      return;
    }
    if (map.isStyleLoaded()) {
      publish(map, data);
    } else {
      map.once("load", () => publish(map, data));
    }
  }, [data]);

  return <div ref={container} className="absolute inset-0 h-full w-full" style={{ position: "absolute", inset: 0 }} />;
}

function classColorExpression(): maplibregl.ExpressionSpecification {
  const expression: unknown[] = ["match", ["get", "className"]];
  for (const [name, color] of Object.entries(CLASS_COLOR)) {
    expression.push(name, color);
  }
  expression.push("#8d98a3");
  return expression as maplibregl.ExpressionSpecification;
}

function publish(map: Map, data: GeoJSON.FeatureCollection) {
  const source = map.getSource("network") as GeoJSONSource | undefined;
  if (!source) {
    return;
  }
  source.setData(data);
  const coordinates = data.features.flatMap((feature) =>
    feature.geometry.type === "LineString" ? feature.geometry.coordinates : [],
  );
  if (coordinates.length === 0) {
    return;
  }
  let minLon = Infinity;
  let minLat = Infinity;
  let maxLon = -Infinity;
  let maxLat = -Infinity;
  for (const [lon, lat] of coordinates) {
    minLon = Math.min(minLon, lon);
    maxLon = Math.max(maxLon, lon);
    minLat = Math.min(minLat, lat);
    maxLat = Math.max(maxLat, lat);
  }
  map.fitBounds(
    [
      [minLon, minLat],
      [maxLon, maxLat],
    ],
    { padding: { left: 400, right: 48, top: 48, bottom: 48 }, duration: 700 },
  );
}
