// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Timing metrics really are published as histograms.
//!
//! Declaring bucket boundaries is what turns a histogram from a rolling quantile summary
//! into a series a percentile can be taken from. Get it wrong and nothing complains: the
//! metric is still exported, still has a mean, and every dashboard percentile and every
//! alert built on one silently returns nothing at all. So this asserts on the exported
//! text, which is the only place the distinction is visible.
//!
//! Its own test binary because the recorder is process-global and installed once. Nothing
//! here reads a *value*: values race every other test in the same process, whereas the
//! shape of the exposition does not.

/// A recorded observation shows up under the right series shape.
#[test]
fn a_timing_metric_is_exported_with_buckets() {
    fosnie_backend::metrics::init();

    metrics::histogram!("voice_turn_latency_seconds", "transport" => "phone").record(0.42);
    metrics::histogram!("http_request_duration_seconds", "method" => "GET", "route" => "/x", "status" => "200")
        .record(0.01);
    metrics::histogram!("voice_frame_rms", "transport" => "phone", "phase" => "capture").record(0.02);

    let text = fosnie_backend::metrics::render();
    assert!(!text.is_empty(), "nothing was exported at all");

    for series in [
        "voice_turn_latency_seconds_bucket",
        "http_request_duration_seconds_bucket",
        "voice_frame_rms_bucket",
    ] {
        assert!(
            text.contains(series),
            "{series} is missing, so a percentile over it returns nothing:\n{text}"
        );
    }

    // A summary is what an undeclared histogram becomes, and it is what the dashboards
    // cannot use. Its giveaway is a quantile label.
    assert!(
        !text.contains("voice_turn_latency_seconds{quantile="),
        "the turn latency is still a summary rather than a histogram"
    );

    // The documented targets have to be boundaries in the exported series, not just in
    // the table that declares them.
    assert!(text.contains("voice_turn_latency_seconds_bucket") && text.contains("le=\"0.8\""));
}
