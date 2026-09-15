//! Safe, optional OpenCode model metadata. No SDK, URL, headers or credentials.
use std::collections::BTreeMap;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<ModelLimit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModelModalities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<BTreeMap<String, ReasoningVariant>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<NonZeroU32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<NonZeroU32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<NonZeroU32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelModalities {
    pub input: Vec<Modality>,
    pub output: Vec<Modality>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Pdf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningVariant {
    pub reasoning_effort: String,
}

impl OpenCodeModelConfig {
    pub(super) fn from_catalog(
        name: Option<String>,
        context: Option<u32>,
        output: Option<u32>,
        capabilities: &[String],
        efforts: &[String],
    ) -> Self {
        let has = |cap: &str| capabilities.iter().any(|value| value == cap);
        let mut input = vec![Modality::Text];
        let mut modalities_output = vec![Modality::Text];
        for (cap, modality) in [
            ("vision", Modality::Image),
            ("pdf", Modality::Pdf),
            ("audioInput", Modality::Audio),
            ("videoInput", Modality::Video),
        ] {
            if has(cap) {
                input.push(modality);
            }
        }
        if has("imageOutput") {
            modalities_output.push(Modality::Image);
        }
        if has("audioOutput") {
            modalities_output.push(Modality::Audio);
        }
        Self {
            name,
            limit: (context.is_some() || output.is_some()).then_some(ModelLimit {
                context: context.and_then(NonZeroU32::new),
                input: None,
                output: output.and_then(NonZeroU32::new),
            }),
            modalities: (!capabilities.is_empty()).then_some(ModelModalities {
                input,
                output: modalities_output,
            }),
            reasoning: (has("reasoning") || !efforts.is_empty()).then_some(true),
            tool_call: has("tools").then_some(true),
            variants: (!efforts.is_empty()).then(|| {
                efforts
                    .iter()
                    .map(|effort| {
                        (
                            effort.clone(),
                            ReasoningVariant {
                                reasoning_effort: effort.clone(),
                            },
                        )
                    })
                    .collect()
            }),
        }
    }

    pub(super) fn overlay(&mut self, other: Self) {
        if other.name.is_some() {
            self.name = other.name;
        }
        if let Some(limit) = other.limit {
            let current = self.limit.get_or_insert_with(ModelLimit::default);
            current.context = limit.context.or(current.context);
            current.input = limit.input.or(current.input);
            current.output = limit.output.or(current.output);
        }
        if other.modalities.is_some() {
            self.modalities = other.modalities;
        }
        if other.reasoning.is_some() {
            self.reasoning = other.reasoning;
        }
        if other.tool_call.is_some() {
            self.tool_call = other.tool_call;
        }
        if other.variants.is_some() {
            self.variants = other.variants;
        }
    }
}
