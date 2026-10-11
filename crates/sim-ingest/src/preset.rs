use sim_core::BBox;

pub const THESSALONIKI_ID: &str = "thessaloniki";

#[derive(Clone, Copy, Debug)]
pub struct CityPreset {
    pub id: &'static str,
    pub label: &'static str,
    /// Inclusive WGS84 box. Thessaloniki covers the historic centre, Kalamaria, Toumba,
    /// Sykies, Evosmos, Ampelokipoi, and the inner ring approaches.
    pub bbox: BBox,
}

const PRESETS: &[CityPreset] = &[CityPreset {
    id: THESSALONIKI_ID,
    label: "Thessaloniki",
    bbox: BBox {
        min_lat: 40.5700,
        min_lon: 22.8850,
        max_lat: 40.6750,
        max_lon: 23.0150,
    },
}];

pub fn presets() -> &'static [CityPreset] {
    PRESETS
}

pub fn preset_by_id(id: &str) -> Option<CityPreset> {
    PRESETS.iter().copied().find(|preset| preset.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thessaloniki_box_is_a_valid_urban_extent() {
        let preset = preset_by_id(THESSALONIKI_ID).unwrap();
        assert!(preset.bbox.validate().is_ok());
        assert!(preset.bbox.min_lat < 40.63 && preset.bbox.max_lat > 40.63);
        assert!(preset.bbox.min_lon < 22.95 && preset.bbox.max_lon > 22.95);
    }
}
