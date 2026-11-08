//! Currency conversion with European Central Bank reference rates, via
//! Frankfurter (no API key).

use serde::Deserialize;

use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

/// Every currency the ECB publishes a reference rate for.
const SUPPORTED: &[&str] = &[
    "AUD", "BRL", "CAD", "CHF", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "IDR", "ILS",
    "INR", "ISK", "JPY", "KRW", "MXN", "MYR", "NOK", "NZD", "PHP", "PLN", "RON", "SEK", "SGD",
    "THB", "TRY", "USD", "ZAR",
];

pub struct ConvertCurrency;

impl BeanTool for ConvertCurrency {
    fn name(&self) -> &'static str {
        "convertCurrency"
    }

    fn description(&self) -> &'static str {
        "Converts an amount of money between currencies using today's exchange rate. Use it \
         whenever someone asks how much something is in another currency."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[
            Param::number("amount", "How much money, e.g. 100"),
            Param::text("from", "Currency to convert from, e.g. 'USD'"),
            Param::text("to", "Currency to convert to, e.g. 'KRW'"),
        ];
        PARAMS
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let amount = args.number("amount").unwrap_or(1.0);
        let from = currency(args.require_text("from")?)?;
        let to = currency(args.require_text("to")?)?;
        ctx.record(format!("Converted {from} to {to}"));
        if from == to {
            return Ok(format!("{} {from}", format_amount(amount)));
        }

        let rates = ctx
            .block_on(latest(ctx.web(), from, to))?
            .map_err(|e| ToolError::new(format!("the exchange rate service failed: {e}")))?;
        let rate = rates
            .rates
            .get(to)
            .copied()
            .ok_or_else(|| ToolError::new(format!("no rate from {from} to {to}")))?;
        Ok(format!(
            "{} {from} = {} {to} (1 {from} = {rate} {to}, ECB reference rate from {})",
            format_amount(amount),
            format_amount(amount * rate),
            rates.date
        ))
    }
}

#[derive(Deserialize)]
struct Rates {
    date: String,
    rates: std::collections::HashMap<String, f64>,
}

async fn latest(web: &reqwest::Client, from: &str, to: &str) -> reqwest::Result<Rates> {
    web.get("https://api.frankfurter.dev/v1/latest")
        .query(&[("base", from), ("symbols", to)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}

/// The ISO code for `name`, which may be a code ("usd"), a symbol ("€"),
/// or a common name ("won").
fn currency(name: &str) -> Result<&'static str, ToolError> {
    let name = name.trim().to_lowercase();
    let code = match name.as_str() {
        "$" | "dollar" | "dollars" | "us dollar" | "us dollars" => "USD",
        "€" | "euro" | "euros" => "EUR",
        "₩" | "won" | "korean won" => "KRW",
        "¥" | "yen" | "japanese yen" => "JPY",
        "£" | "pound" | "pounds" | "sterling" => "GBP",
        "yuan" | "rmb" | "renminbi" => "CNY",
        "rupee" | "rupees" => "INR",
        "franc" | "swiss franc" => "CHF",
        "baht" => "THB",
        "peso" | "pesos" => "MXN",
        "real" | "reais" => "BRL",
        other => other,
    };
    let code = code.to_uppercase();
    SUPPORTED
        .iter()
        .find(|supported| **supported == code)
        .copied()
        .ok_or_else(|| {
            ToolError::new(format!(
                "unsupported currency {name:?}; supported: {}",
                SUPPORTED.join(", ")
            ))
        })
}

/// Formats money readably: "1,358,730" for large amounts, "12.50" for
/// small ones.
fn format_amount(amount: f64) -> String {
    let decimals = if amount >= 1000.0 { 0 } else { 2 };
    let text = format!("{amount:.decimals$}");
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text.as_str(), None), |(w, f)| (w, Some(f)));
    let digits: Vec<char> = whole.chars().collect();
    let mut grouped = String::new();
    for (i, digit) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*digit);
    }
    match fraction {
        Some(fraction) => format!("{grouped}.{fraction}"),
        None => grouped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn understands_codes_symbols_and_names() {
        assert_eq!(currency("usd"), Ok("USD"));
        assert_eq!(currency(" Won "), Ok("KRW"));
        assert_eq!(currency("€"), Ok("EUR"));
        assert_eq!(currency("Japanese yen"), Ok("JPY"));
    }

    #[test]
    fn rejects_unknown_currencies() {
        let error = currency("bitcoin").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported currency \"bitcoin\"")
        );
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(format_amount(12.5), "12.50");
        assert_eq!(format_amount(999.999), "1,000.00");
        assert_eq!(format_amount(135873.0), "135,873");
        assert_eq!(format_amount(1234567.4), "1,234,567");
        assert_eq!(format_amount(1000.0), "1,000");
    }

    #[test]
    fn parses_rates() {
        let json = r#"{"amount":1.0,"base":"USD","date":"2026-11-06","rates":{"KRW":1358.73}}"#;
        let rates: Rates = serde_json::from_str(json).unwrap();
        assert_eq!(rates.date, "2026-11-06");
        assert_eq!(rates.rates["KRW"], 1358.73);
    }
}
