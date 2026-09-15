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
    pub attachment: Option<bool>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

pub(super) struct ModelMetadataFacts<'a> {
    pub name: Option<String>,
    pub context: Option<u32>,
    pub input: Option<u32>,
    pub output: Option<u32>,
    pub capabilities: &'a [String],
    pub modalities: Option<(&'a [String], &'a [String])>,
    pub attachment: Option<bool>,
    pub reasoning: Option<bool>,
    pub tool_call: Option<bool>,
    pub efforts: Option<&'a [String]>,
}

impl OpenCodeModelConfig {
    pub(super) fn from_facts(facts: ModelMetadataFacts<'_>) -> Self {
        let ModelMetadataFacts {
            name,
            context,
            input,
            output,
            capabilities,
            modalities,
            attachment,
            reasoning,
            tool_call,
            efforts,
        } = facts;
        let has = |cap: &str| capabilities.iter().any(|value| value == cap);
        let modalities = modalities
            .map(|(input, output)| ModelModalities {
                input: input
                    .iter()
                    .filter_map(|value| Modality::parse(value))
                    .collect(),
                output: output
                    .iter()
                    .filter_map(|value| Modality::parse(value))
                    .collect(),
            })
            .or_else(|| {
                (!capabilities.is_empty()).then(|| {
                    let mut input = vec![Modality::Text];
                    let mut output = vec![Modality::Text];
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
                        output.push(Modality::Image);
                    }
                    if has("audioOutput") {
                        output.push(Modality::Audio);
                    }
                    ModelModalities { input, output }
                })
            });
        Self {
            name,
            limit: (context.is_some() || input.is_some() || output.is_some()).then_some(
                ModelLimit {
                    context: context.and_then(NonZeroU32::new),
                    input: input.and_then(NonZeroU32::new),
                    output: output.and_then(NonZeroU32::new),
                },
            ),
            modalities,
            attachment,
            reasoning: reasoning.or_else(|| {
                (has("reasoning") || efforts.is_some_and(|values| !values.is_empty()))
                    .then_some(true)
            }),
            tool_call: tool_call.or_else(|| has("tools").then_some(true)),
            variants: efforts.map(|values| {
                values
                    .iter()
                    .map(|effort| {
                        (
                            effort.clone(),
                            ReasoningVariant {
                                reasoning_effort: Some(effort.clone()),
                                disabled: None,
                            },
                        )
                    })
                    .collect()
            }),
        }
    }
}

impl Modality {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}
impl OpenCodeModelConfig {
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
        if other.attachment.is_some() {
            self.attachment = other.attachment;
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
