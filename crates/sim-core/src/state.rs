use serde::{Deserialize, Serialize};

use crate::graph::Network;
use crate::id::EdgeId;

/// Residuals smaller than this are treated as an empty link in the density view.
/// Larger deficits stay visible so a conservation bug cannot be clamped away.
pub const CONSERVATION_EPSILON: f64 = 1e-8;

/// Link-transmission accumulators. One entry per edge, indexed by `EdgeId`.
///
/// `vehicles = cumulative_in - cumulative_out`. Nothing in this type removes a
/// vehicle to clear a queue. `cumulative_demand` counts vehicles assigned to the
/// link that are still held upstream because residual storage is zero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkTransmissionState {
    pub cumulative_in: Vec<f64>,
    pub cumulative_out: Vec<f64>,
    pub cumulative_demand: Vec<f64>,
}

impl LinkTransmissionState {
    pub fn empty(edge_count: usize) -> Self {
        Self {
            cumulative_in: vec![0.0; edge_count],
            cumulative_out: vec![0.0; edge_count],
            cumulative_demand: vec![0.0; edge_count],
        }
    }

    pub fn from_network(network: &Network) -> Self {
        Self::empty(network.edges.len())
    }

    pub fn conservation_residual(&self, edge: EdgeId) -> f64 {
        let index = edge.index();
        self.cumulative_in[index] - self.cumulative_out[index]
    }

    /// Vehicle count used by density and storage. A sub-epsilon residual is zero.
    /// A material negative residual is returned unchanged.
    pub fn vehicles(&self, edge: EdgeId) -> f64 {
        let residual = self.conservation_residual(edge);
        if residual.abs() < CONSERVATION_EPSILON {
            0.0
        } else {
            residual
        }
    }

    pub fn available_storage(&self, network: &Network, edge: EdgeId) -> f64 {
        storage_inflow_ceiling(
            network.edges[edge.index()].storage_max_veh,
            self.vehicles(edge),
        )
    }

    pub fn conserved(&self) -> bool {
        self.cumulative_in
            .iter()
            .zip(&self.cumulative_out)
            .all(|(entered, left)| entered - left >= -CONSERVATION_EPSILON)
    }
}

/// Vehicle increment that residual storage can still accept.
/// This is the project bound `Inflow_e(t) <= max(0, S_max,e - N_e(t))`.
/// The time stepper divides by `Δt` when it needs a rate. Discharge capacity is a separate bound.
pub fn storage_inflow_ceiling(storage_max_veh: f64, vehicles_on_link: f64) -> f64 {
    (storage_max_veh - vehicles_on_link).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_link_can_accept_its_whole_storage() {
        assert!((storage_inflow_ceiling(120.0, 0.0) - 120.0).abs() < 1e-12);
    }

    #[test]
    fn jammed_link_accepts_nothing_and_does_not_go_negative() {
        assert_eq!(storage_inflow_ceiling(120.0, 120.0), 0.0);
        assert_eq!(storage_inflow_ceiling(120.0, 140.0), 0.0);
    }

    #[test]
    fn epsilon_residual_reads_as_empty_but_a_lost_vehicle_does_not() {
        let mut state = LinkTransmissionState::empty(1);
        state.cumulative_out[0] = 1e-12;
        assert_eq!(state.vehicles(EdgeId(0)), 0.0);
        state.cumulative_out[0] = 1.0;
        assert!((state.vehicles(EdgeId(0)) + 1.0).abs() < 1e-12);
        assert!(!state.conserved());
    }
}
