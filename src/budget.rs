use std::fmt;

use crate::config::TokenConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetStatus {
    Within,
    SoftExceeded,
    HardExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetUsage {
    pub soft_budget: usize,
    pub hard_budget: usize,
    pub raw_tokens: usize,
    pub packet_tokens: usize,
    pub status: BudgetStatus,
}

impl BudgetUsage {
    pub fn from_config(config: &TokenConfig, raw_tokens: usize, packet_tokens: usize) -> Self {
        let soft_budget = config.soft_budget as usize;
        let hard_budget = config.hard_budget as usize;
        let status = if packet_tokens > hard_budget {
            BudgetStatus::HardExceeded
        } else if packet_tokens > soft_budget {
            BudgetStatus::SoftExceeded
        } else {
            BudgetStatus::Within
        };

        Self {
            soft_budget,
            hard_budget,
            raw_tokens,
            packet_tokens,
            status,
        }
    }

    pub fn hard_error(&self) -> Option<BudgetExceededError> {
        (self.status == BudgetStatus::HardExceeded).then_some(BudgetExceededError { usage: *self })
    }

    pub fn render(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!(
            "Budget:  soft: {}  hard: {}\n",
            format_count(self.soft_budget),
            format_count(self.hard_budget)
        ));
        output.push_str(&format!(
            "This run: raw output: {} ({:.1}% of soft, {:.1}% of hard)  packet: {} ({:.1}% of soft, {:.1}% of hard)\n",
            format_count(self.raw_tokens),
            percent_used(self.raw_tokens, self.soft_budget),
            percent_used(self.raw_tokens, self.hard_budget),
            format_count(self.packet_tokens),
            percent_used(self.packet_tokens, self.soft_budget),
            percent_used(self.packet_tokens, self.hard_budget)
        ));
        output.push_str(&format!("Status: {}\n", self.status_message()));

        output
    }

    fn status_message(&self) -> String {
        match self.status {
            BudgetStatus::Within => format!(
                "packet is within budget; raw output would have used {:.1}% of soft budget",
                percent_used(self.raw_tokens, self.soft_budget)
            ),
            BudgetStatus::SoftExceeded => format!(
                "warning: packet exceeds soft budget ({:.1}% used)",
                percent_used(self.packet_tokens, self.soft_budget)
            ),
            BudgetStatus::HardExceeded => format!(
                "error: packet exceeds hard budget ({:.1}% used); rerun with --force to continue",
                percent_used(self.packet_tokens, self.hard_budget)
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetExceededError {
    usage: BudgetUsage,
}

impl fmt::Display for BudgetExceededError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.usage.status_message())
    }
}

impl std::error::Error for BudgetExceededError {}

fn percent_used(tokens: usize, budget: usize) -> f64 {
    if budget == 0 {
        return 0.0;
    }

    tokens as f64 / budget as f64 * 100.0
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_config() -> TokenConfig {
        TokenConfig {
            soft_budget: 40_000,
            hard_budget: 80_000,
        }
    }

    #[test]
    fn reports_percentages_and_within_budget_status() {
        let usage = BudgetUsage::from_config(&token_config(), 11_010, 740);
        let rendered = usage.render();

        assert_eq!(usage.status, BudgetStatus::Within);
        assert!(rendered.contains("Budget:  soft: 40,000  hard: 80,000"));
        assert!(rendered.contains("raw output: 11,010 (27.5% of soft, 13.8% of hard)"));
        assert!(rendered.contains("packet: 740 (1.8% of soft, 0.9% of hard)"));
        assert!(rendered.contains("Status: packet is within budget"));
        assert!(rendered.contains("raw output would have used 27.5% of soft budget"));
        assert!(usage.hard_error().is_none());
    }

    #[test]
    fn warns_when_packet_exceeds_soft_budget() {
        let usage = BudgetUsage::from_config(&token_config(), 100_000, 50_000);

        assert_eq!(usage.status, BudgetStatus::SoftExceeded);
        assert!(
            usage
                .render()
                .contains("warning: packet exceeds soft budget")
        );
        assert!(usage.hard_error().is_none());
    }

    #[test]
    fn errors_when_packet_exceeds_hard_budget() {
        let usage = BudgetUsage::from_config(&token_config(), 100_000, 90_000);
        let error = usage.hard_error().expect("hard budget should be exceeded");

        assert_eq!(usage.status, BudgetStatus::HardExceeded);
        assert!(error.to_string().contains("packet exceeds hard budget"));
        assert!(usage.render().contains("rerun with --force"));
    }
}
