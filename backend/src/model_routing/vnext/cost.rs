// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::deployments::ModelTariffs;
pub fn reservation_pricing(t: &ModelTariffs) -> super::super::ModelPricing {
    let mut p = t.pricing.clone();
    if let (Some(input), Some(cached)) = (
        p.input_microusd_per_million,
        t.cached_input_microusd_per_million,
    ) {
        p.input_microusd_per_million = Some(input.max(cached));
    }
    if let (Some(output), Some(reasoning)) = (
        p.output_microusd_per_million,
        t.reasoning_output_microusd_per_million,
    ) {
        p.output_microusd_per_million = Some(output.max(reasoning));
    }
    p
}
