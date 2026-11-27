use serde::{Deserialize, Serialize};
use term_config::{ClaudeConfig, ModelPrice};

use crate::model::{AssistantRecord, CostStateRecord};

const TOKENS_PER_MILLION: f64 = 1_000_000.0;

/// Origin of a session's cost calculation (CONTRATO §B.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CostSource {
    CostState,
    Estimated,
    None,
}

/// Computed cost summary for one Claude session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionCost {
    pub cost_usd: f64,
    pub cost_source: CostSource,
    pub cost_incomplete: bool,
}

/// Computes `costUsd`, `costSource`, and `costIncomplete` per CONTRATO §A.4, §B.6, and §F.2.
///
/// Priority:
/// 1. If a `cost-state` record is present, use its `totalCostUSD` (`costSource = "cost-state"`).
///    `costIncomplete` is `hasUnknownModelCost` (or `true` if `totalCostUSD` was missing).
/// 2. Otherwise, estimate across deduplicated `assistant` messages using `[term.claude.prices]`.
///    If at least one message's model has a configured price, `costSource = "estimated"`;
///    otherwise `costSource = "none"`. Any message with a missing/unpriced model marks
///    `costIncomplete = true`.
pub fn compute_session_cost(
    assistants: &[AssistantRecord],
    last_cost_state: Option<&CostStateRecord>,
    config: &ClaudeConfig,
) -> SessionCost {
    if let Some(cs) = last_cost_state {
        return match cs.total_cost_usd {
            Some(total) => SessionCost {
                cost_usd: total,
                cost_source: CostSource::CostState,
                cost_incomplete: cs.has_unknown_model_cost,
            },
            None => SessionCost {
                cost_usd: 0.0,
                cost_source: CostSource::CostState,
                cost_incomplete: true,
            },
        };
    }

    if assistants.is_empty() {
        return SessionCost {
            cost_usd: 0.0,
            cost_source: CostSource::None,
            cost_incomplete: false,
        };
    }

    let mut total_usd = 0.0_f64;
    let mut any_priced = false;
    let mut missing_price = false;

    for msg in assistants {
        let Some(model) = msg.model.as_deref() else {
            missing_price = true;
            continue;
        };
        let Some(price) = config.price_for(model) else {
            missing_price = true;
            continue;
        };
        any_priced = true;
        total_usd += estimate_message_cost(msg, price);
    }

    if !any_priced {
        return SessionCost {
            cost_usd: 0.0,
            cost_source: CostSource::None,
            cost_incomplete: missing_price,
        };
    }

    SessionCost {
        cost_usd: total_usd,
        cost_source: CostSource::Estimated,
        cost_incomplete: missing_price,
    }
}

fn estimate_message_cost(msg: &AssistantRecord, price: &ModelPrice) -> f64 {
    let input_cost = (msg.tokens.input as f64) * price.input / TOKENS_PER_MILLION;
    let output_cost = (msg.tokens.output as f64) * price.output / TOKENS_PER_MILLION;
    let cache_read_cost = (msg.tokens.cache_read as f64) * price.cache_read / TOKENS_PER_MILLION;

    let cache_write_cost = if msg.cache_write_5m > 0 || msg.cache_write_1h > 0 {
        (msg.cache_write_5m as f64) * price.cache_write_5m / TOKENS_PER_MILLION
            + (msg.cache_write_1h as f64) * price.cache_write_1h / TOKENS_PER_MILLION
    } else {
        (msg.tokens.cache_write as f64) * price.cache_write_5m / TOKENS_PER_MILLION
    };

    input_cost + output_cost + cache_read_cost + cache_write_cost
}
