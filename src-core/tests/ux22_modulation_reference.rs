use serde_json::Value;
use shocking_vrc_core::{
    modulation::{
        evaluator::{advance_accumulator, evaluate_with_accumulator},
        KinematicsInput, ModulationConfig, ModulationFunction, ModulationSource,
    },
    raw_to_hz,
};

const FIXTURE: &str = include_str!("../../src-terminal/tests/fixtures/modulation-parity-v1.json");

fn function_from_api(value: &Value) -> ModulationFunction {
    match value["kind"].as_str().expect("function kind") {
        "sin" => ModulationFunction::Sin,
        "cos" => ModulationFunction::Cos,
        "perlin" => ModulationFunction::Perlin,
        "simplex" => ModulationFunction::Simplex,
        "compose" => ModulationFunction::Compose {
            outer: Box::new(function_from_api(&value["outer"])),
            inner: Box::new(function_from_api(&value["inner"])),
        },
        kind => panic!("unexpected fixture function: {kind}"),
    }
}

fn number(value: &Value, key: &str) -> f32 {
    value[key].as_f64().expect("numeric modulation field") as f32
}

#[test]
fn host_modulation_matches_the_shared_terminal_numeric_fixture() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("valid shared modulation fixture");
    let source_value = number(&fixture, "sourceValue");
    let sample_count = fixture["sampleCount"].as_u64().expect("sample count") as usize;
    let step_seconds = number(&fixture, "stepSeconds");
    let indices: Vec<usize> = fixture["indices"]
        .as_array()
        .expect("sample indices")
        .iter()
        .map(|value| value.as_u64().expect("sample index") as usize)
        .collect();

    for row in fixture["cases"].as_array().expect("cases") {
        let config = &row["config"];
        let modulation = ModulationConfig {
            source: ModulationSource::Depth,
            function: function_from_api(&config["function"]),
            base_speed: number(config, "base_speed"),
            sensitivity: number(config, "sensitivity"),
            max_deviation: number(config, "max_deviation"),
            phase: number(config, "phase"),
            frequency_multiplier: number(config, "frequency_multiplier"),
            offset: number(config, "offset"),
            power: number(config, "power"),
            clamp_min: number(config, "clamp_min"),
            clamp_max: number(config, "clamp_max"),
        };
        let base_value = number(row, "baseValue");
        let mut accumulator = 0.0;
        let mut host_values = Vec::new();
        let mut display_hz = Vec::new();
        for index in 0..sample_count {
            advance_accumulator(
                &modulation,
                &mut accumulator,
                &KinematicsInput {
                    depth: source_value,
                    ..Default::default()
                },
                step_seconds,
            );
            let value = evaluate_with_accumulator(&modulation, base_value, accumulator);
            if indices.contains(&index) {
                host_values.push(value);
                if row["kind"].as_str() == Some("freq") {
                    display_hz.push(raw_to_hz(value.clamp(10.0, 255.0) as u8));
                }
            }
        }

        for (index, (actual, expected)) in host_values
            .iter()
            .zip(row["hostValues"].as_array().expect("expected Host samples"))
            .enumerate()
        {
            let expected = expected.as_f64().expect("Host sample") as f32;
            assert!(
                (actual - expected).abs() <= 0.0001,
                "{} Host sample {}: {actual} vs {expected}",
                row["name"],
                indices[index]
            );
        }
        if row["kind"].as_str() == Some("freq") {
            for (index, (actual, expected)) in display_hz
                .iter()
                .zip(row["displayHz"].as_array().expect("expected frequency display samples"))
                .enumerate()
            {
                let expected = expected.as_f64().expect("display Hz sample") as f32;
                assert!(
                    (actual - expected).abs() <= 0.00001,
                    "{} Hz sample {}: {actual} vs {expected}",
                    row["name"],
                    indices[index]
                );
            }
        }
    }
}
