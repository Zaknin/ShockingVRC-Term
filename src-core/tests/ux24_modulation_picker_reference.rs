use std::str::FromStr;

use serde_json::Value;
use shocking_vrc_core::modulation::{
    evaluator::{advance_accumulator, evaluate_with_accumulator},
    KinematicsInput, ModulationConfig, ModulationFunction,
};

const FIXTURE: &str = include_str!("../../src-terminal/src/domain/modulation-picker-sparklines.json");

#[test]
fn terminal_picker_sparklines_match_host_evaluator_for_every_standard_function() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("valid picker sparkline fixture");
    assert_eq!(fixture["sampleCount"].as_u64(), Some(64));
    let rows = fixture["functions"].as_array().expect("function sample rows");
    assert_eq!(rows.len(), 44, "every standard Host picker function is covered");

    for row in rows {
        let name = row["name"].as_str().expect("Host function label");
        let function = ModulationFunction::from_str(name)
            .unwrap_or_else(|error| panic!("{name} must parse as a Host function: {error}"));
        let config = ModulationConfig {
            function,
            base_speed: 1.0,
            sensitivity: 0.0,
            max_deviation: 1.0,
            phase: 0.0,
            frequency_multiplier: 1.0,
            offset: 0.0,
            power: 1.0,
            clamp_min: -10.0,
            clamp_max: 10.0,
            ..ModulationConfig::default()
        };
        let mut accumulator = 0.0_f32;
        let mut values = Vec::with_capacity(64);
        let kinematics = KinematicsInput::default();
        for _ in 0..64 {
            advance_accumulator(&config, &mut accumulator, &kinematics, 1.0 / 64.0);
            values.push(evaluate_with_accumulator(&config, 0.0, accumulator));
        }

        let low = values.iter().copied().fold(f32::INFINITY, f32::min);
        let high = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let span = (high - low).max(1e-3);
        let expected = row["normalized"].as_array().expect("normalized samples");
        assert_eq!(expected.len(), 64, "{name} sample count");
        for (index, (value, expected)) in values.iter().zip(expected).enumerate() {
            let normalized = (*value - low) / span;
            let expected = expected.as_f64().expect("normalized sample") as f32;
            assert!(
                (normalized - expected).abs() <= 0.00002,
                "{name} sparkline sample {index}: Rust {normalized} vs Terminal {expected}"
            );
        }
    }
}
