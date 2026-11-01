use serde::{Deserialize, Serialize};

use crate::types::FieldReading;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenField {
    pub id: String,
    pub file: String,
    pub page: u32,
    pub metric: String,
    pub raw: String,
    pub value: Option<f64>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldScore {
    pub id: String,
    pub hit: bool,
}

pub fn field_exact_match(expected: &GoldenField, actual: &FieldReading) -> bool {
    if !expected.confirmed {
        return false;
    }
    normalize(&expected.raw) == normalize(&actual.raw)
}

pub fn replay(gold: &[GoldenField], actual: &[FieldReading]) -> Result<Vec<FieldScore>, String> {
    let mut scores = Vec::new();
    for field in gold.iter().filter(|g| g.confirmed) {
        let hit = actual
            .iter()
            .any(|a| a.metric == field.metric && normalize(&a.raw) == normalize(&field.raw));
        if !hit {
            return Err(format!(
                "regression: {} {} on {} p.{} no longer matches {}",
                field.metric, field.raw, field.file, field.page, field.id
            ));
        }
        scores.push(FieldScore {
            id: field.id.clone(),
            hit: true,
        });
    }
    Ok(scores)
}

/// Prior 41744 readings. They stay unconfirmed until the user marks them in
/// the parse view, so they cannot score a new pipeline against old mistakes.
pub fn seed_41744() -> Vec<GoldenField> {
    vec![
        GoldenField {
            id: "41744-tsn".into(),
            file: "41744_#01 BLADE_RGF34904.pdf".into(),
            page: 4,
            metric: "tsn".into(),
            raw: "29,189:16".into(),
            value: Some(29189.2667),
            confirmed: false,
        },
        GoldenField {
            id: "41744-csn".into(),
            file: "41744_#01 BLADE_RGF34904.pdf".into(),
            page: 4,
            metric: "csn".into(),
            raw: "4,585".into(),
            value: Some(4585.0),
            confirmed: false,
        },
        GoldenField {
            id: "41744-limit".into(),
            file: "41744_#01 BLADE_RGF34904.pdf".into(),
            page: 4,
            metric: "life_limit_cycles".into(),
            raw: "14,336".into(),
            value: Some(14336.0),
            confirmed: false,
        },
        GoldenField {
            id: "41744-sn".into(),
            file: "41744_#01 BLADE_RGF34904.pdf".into(),
            page: 1,
            metric: "serial_number".into(),
            raw: "RGF34904".into(),
            value: None,
            confirmed: false,
        },
    ]
}

fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect::<String>()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfirmed_seed_is_not_gold() {
        let seed = seed_41744();
        let actual = FieldReading {
            metric: "tsn".into(),
            raw: "29,189:16".into(),
            value: Some(29189.2667),
            bbox: None,
            confidence: 1.0,
            provider: "engine".into(),
        };
        assert!(!field_exact_match(&seed[0], &actual));
        assert!(replay(&seed, &[actual]).unwrap().is_empty());
    }

    #[test]
    fn confirmed_miss_blocks_release() {
        let mut gold = seed_41744();
        gold[1].confirmed = true;
        let actual = FieldReading {
            metric: "csn".into(),
            raw: "4586".into(),
            value: Some(4586.0),
            bbox: None,
            confidence: 1.0,
            provider: "engine".into(),
        };
        assert!(replay(&gold, &[actual]).is_err());
    }
}
